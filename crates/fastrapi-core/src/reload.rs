//! The supervisor for automatic reload in development.
//! It starts the server as a child process and watches Python files for changes.
//! When a file that is not ignored changes, it stops the child process and starts a new one.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use std::env::current_dir;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// How long a stopping child gets to run its lifespan shutdown before it is killed.
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest the supervisor waits between checks of the child and of its own signals.
const MAX_POLL: Duration = Duration::from_millis(100);
/// The first and the longest delay before a crashed child is started again.
const RESPAWN_BACKOFF_MIN: Duration = Duration::from_millis(250);
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(8);
/// A child that ran this long before it exited resets the backoff.
const RESPAWN_BACKOFF_RESET: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct ReloadConfig {
    pub watch_dirs: Vec<PathBuf>,
    pub ignore_dirs: Vec<PathBuf>,
    pub ignore_patterns: Vec<String>,
    pub ignore_paths: Vec<PathBuf>,
    /// The quiet period after a change before the child restarts, so one save that writes
    /// several files (or writes then renames) restarts it once.
    pub tick_ms: u64,
    pub ignore_worker_failure: bool,
}

pub fn run_reload_supervisor(
    executable: &str,
    argv: &[String],
    config: &ReloadConfig,
) -> Result<(), String> {
    let ignore_globs = build_reload_ignore_globs(config)?;
    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            _ = tx.send(event);
        },
        NotifyConfig::default(),
    )
    .map_err(|err| err.to_string())?;

    for dir in &config.watch_dirs {
        watcher
            .watch(dir, RecursiveMode::Recursive)
            .map_err(|err| err.to_string())?;
    }

    let signals = signals::Guard::install();
    let debounce = Duration::from_millis(config.tick_ms);
    let poll = debounce.min(MAX_POLL);
    let mut child = Some(spawn_reload_child(executable, argv).map_err(|err| err.to_string())?);
    let mut started = Instant::now();
    let mut backoff = RESPAWN_BACKOFF_MIN;
    let mut respawn_at: Option<Instant> = None;
    let mut last_change: Option<Instant> = None;

    loop {
        if signals.interrupted() {
            if let Some(child) = child.as_mut() {
                // A console Ctrl+C on Windows already reached the child, as uvicorn relies on;
                // on unix the supervisor forwards SIGTERM, as uvicorn's `shutdown` does.
                if signals::FORWARD_ON_EXIT {
                    stop_child(child);
                } else {
                    wait_then_kill(child);
                }
            }
            return Ok(());
        }

        if let Some(running) = child.as_mut()
            && let Some(status) = running.try_wait().map_err(|err| err.to_string())?
        {
            child = None;
            if status.success() || signals::is_interrupt(status) {
                return Ok(());
            }
            if config.ignore_worker_failure {
                if started.elapsed() >= RESPAWN_BACKOFF_RESET {
                    backoff = RESPAWN_BACKOFF_MIN;
                }
                eprintln!(
                    "FastrAPI reload: child exited with status {status}; restarting in {}ms",
                    backoff.as_millis()
                );
                respawn_at = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(RESPAWN_BACKOFF_MAX);
            } else {
                // As uvicorn's reloader does, keep watching and start again on the next change.
                eprintln!(
                    "FastrAPI reload: child exited with status {status}; waiting for file changes before restarting"
                );
            }
        }

        if child.is_none() && respawn_at.is_some_and(|at| Instant::now() >= at) {
            respawn_at = None;
            child = Some(spawn_reload_child(executable, argv).map_err(|err| err.to_string())?);
            started = Instant::now();
        }

        match rx.recv_timeout(poll) {
            Ok(Ok(event)) => {
                if reload_event_matches(&event, config, ignore_globs.as_ref()) {
                    last_change = Some(Instant::now());
                }
            }
            Ok(Err(err)) => return Err(err.to_string()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("reload watcher stopped".to_string());
            }
        }

        if last_change.is_some_and(|at| at.elapsed() >= debounce) {
            last_change = None;
            println!("Python file change detected, restarting server...");
            if let Some(mut running) = child.take() {
                stop_child(&mut running);
            }
            respawn_at = None;
            backoff = RESPAWN_BACKOFF_MIN;
            child = Some(spawn_reload_child(executable, argv).map_err(|err| err.to_string())?);
            started = Instant::now();
        }
    }
}

fn spawn_reload_child(executable: &str, argv: &[String]) -> io::Result<Child> {
    let mut command = Command::new(executable);
    command
        .args(argv)
        .env("FASTRAPI_RELOAD_CHILD", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command.spawn()
}

/// Asks the child to shut down (SIGTERM on unix, Ctrl+C on Windows, as uvicorn's reloader does)
/// so its lifespan shutdown runs, and kills it only if it is still running after a timeout.
fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    if signals::request_stop(child) {
        wait_then_kill(child);
    } else {
        _ = child.kill();
        _ = child.wait();
    }
}

/// Waits for a child that was asked to stop, and kills it if it outlives the timeout.
fn wait_then_kill(child: &mut Child) {
    let deadline = Instant::now() + GRACEFUL_STOP_TIMEOUT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    eprintln!("FastrAPI reload: child did not stop in time; killing it");
    _ = child.kill();
    _ = child.wait();
}

#[cfg(unix)]
mod signals {
    use super::{Child, ExitStatus};
    use std::os::unix::process::ExitStatusExt;
    use std::sync::atomic::{AtomicI32, Ordering};

    pub const FORWARD_ON_EXIT: bool = true;

    static RECEIVED: AtomicI32 = AtomicI32::new(0);

    extern "C" fn on_signal(signal: libc::c_int) {
        RECEIVED.store(signal, Ordering::SeqCst);
    }

    /// Catches SIGINT and SIGTERM while the supervisor runs, so a Ctrl+C reaches it even while
    /// no child is running, and puts the previous handlers back when dropped.
    pub struct Guard {
        previous: [(libc::c_int, libc::sighandler_t); 2],
    }

    impl Guard {
        pub fn install() -> Self {
            RECEIVED.store(0, Ordering::SeqCst);
            let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            // SAFETY: `on_signal` only stores to an atomic, which is async-signal-safe.
            let previous = [libc::SIGINT, libc::SIGTERM]
                .map(|signal| (signal, unsafe { libc::signal(signal, handler) }));
            Self { previous }
        }

        pub fn interrupted(&self) -> bool {
            _ = self;
            RECEIVED.load(Ordering::SeqCst) != 0
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            for (signal, previous) in self.previous {
                // SAFETY: restores the handler that `install` replaced.
                unsafe { libc::signal(signal, previous) };
            }
            let received = RECEIVED.swap(0, Ordering::SeqCst);
            if received != 0 {
                // Hand the signal to the previous handler, so Python raises KeyboardInterrupt.
                // SAFETY: raising a signal on this process has no memory-safety requirement.
                unsafe { libc::raise(received) };
            }
        }
    }

    pub fn request_stop(child: &Child) -> bool {
        let Ok(pid) = libc::pid_t::try_from(child.id()) else {
            return false;
        };
        // SAFETY: `pid` is our unreaped child, so the id cannot name another process.
        unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
    }

    pub fn is_interrupt(status: ExitStatus) -> bool {
        matches!(status.signal(), Some(libc::SIGINT | libc::SIGTERM))
            || matches!(status.code(), Some(130 | 143))
    }
}

#[cfg(windows)]
mod signals {
    use super::{Child, ExitStatus};
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::System::Console::CTRL_C_EVENT;
    use windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::core::BOOL;

    pub const FORWARD_ON_EXIT: bool = false;

    /// `STATUS_CONTROL_C_EXIT`, the exit code of a process that Ctrl+C ended.
    const CONTROL_C_EXIT: u32 = 0xC000_013A;

    static RESTARTING: AtomicBool = AtomicBool::new(false);
    static INTERRUPTED: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn on_ctrl(ctrl_type: u32) -> BOOL {
        if ctrl_type == CTRL_C_EVENT && RESTARTING.swap(false, Ordering::SeqCst) {
            // The Ctrl+C the supervisor sent its child also reaches the supervisor; swallow it.
            return 1;
        }
        INTERRUPTED.store(true, Ordering::SeqCst);
        // Let the next handler (Python's) see it too.
        0
    }

    /// Watches the console's Ctrl+C while the supervisor runs, and removes the handler when dropped.
    pub struct Guard;

    impl Guard {
        pub fn install() -> Self {
            INTERRUPTED.store(false, Ordering::SeqCst);
            RESTARTING.store(false, Ordering::SeqCst);
            // SAFETY: `on_ctrl` only touches atomics and stays valid for the process's life.
            unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
            Self
        }

        pub fn interrupted(&self) -> bool {
            _ = self;
            INTERRUPTED.load(Ordering::SeqCst)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: removes the handler `install` added.
            unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 0) };
        }
    }

    /// Sends Ctrl+C to the console, as uvicorn's reloader does; the supervisor ignores its own copy.
    pub fn request_stop(_child: &Child) -> bool {
        RESTARTING.store(true, Ordering::SeqCst);
        // SAFETY: a plain Win32 call with no pointer arguments.
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) } != 0;
        if !sent {
            RESTARTING.store(false, Ordering::SeqCst);
        }
        sent
    }

    pub fn is_interrupt(status: ExitStatus) -> bool {
        status
            .code()
            .is_some_and(|code| code.cast_unsigned() == CONTROL_C_EXIT)
    }
}

#[cfg(not(any(unix, windows)))]
mod signals {
    use super::{Child, ExitStatus};

    pub const FORWARD_ON_EXIT: bool = true;

    pub struct Guard;

    impl Guard {
        pub const fn install() -> Self {
            Self
        }

        pub const fn interrupted(&self) -> bool {
            false
        }
    }

    pub const fn request_stop(_child: &Child) -> bool {
        false
    }

    pub const fn is_interrupt(_status: ExitStatus) -> bool {
        false
    }
}

fn reload_event_matches(
    event: &notify::Event,
    config: &ReloadConfig,
    ignore_globs: Option<&GlobSet>,
) -> bool {
    event.paths.iter().any(|path| {
        path.extension().and_then(|ext| ext.to_str()) == Some("py")
            && !is_reload_ignored(path, config, ignore_globs)
    })
}

fn is_reload_ignored(path: &Path, config: &ReloadConfig, ignore_globs: Option<&GlobSet>) -> bool {
    if path.ancestors().any(is_default_reload_ignored_dir) {
        return true;
    }

    if path.ancestors().any(|ancestor| {
        config.ignore_dirs.iter().any(|ignored| {
            ancestor == ignored
                || ancestor.ends_with(ignored)
                || ignored
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| ancestor.file_name().and_then(|n| n.to_str()) == Some(name))
        })
    }) {
        return true;
    }

    if config
        .ignore_paths
        .iter()
        .any(|ignored| path == ignored || path.ends_with(ignored))
    {
        return true;
    }

    ignore_globs.is_some_and(|globs| globs.is_match(path))
}

fn is_default_reload_ignored_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | ".venv" | "__pycache__" | "target"))
}

fn build_reload_ignore_globs(config: &ReloadConfig) -> Result<Option<GlobSet>, String> {
    if config.ignore_patterns.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in config
        .ignore_patterns
        .iter()
        .filter(|pattern| !pattern.is_empty())
    {
        let pattern = if pattern.contains(['*', '?', '[', ']']) {
            pattern.clone()
        } else {
            format!("**/*{pattern}*")
        };
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(false)
            .build()
            .map_err(|err| err.to_string())?;
        builder.add(glob);
    }

    builder.build().map(Some).map_err(|err| err.to_string())
}

pub fn resolve_reload_dirs(script_path: &str, reload_dirs: Option<Vec<String>>) -> Vec<PathBuf> {
    if let Some(dirs) = reload_dirs {
        return dirs.into_iter().map(PathBuf::from).collect();
    }

    let script = PathBuf::from(script_path);
    if let Some(parent) = script.parent()
        && !parent.as_os_str().is_empty()
    {
        return vec![parent.to_path_buf()];
    }

    current_dir().map_or_else(|_| vec![PathBuf::from(".")], |dir| vec![dir])
}
