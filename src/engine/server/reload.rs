use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

#[derive(Clone)]
pub(crate) struct ReloadConfig {
    pub(crate) watch_dirs: Vec<PathBuf>,
    pub(crate) ignore_dirs: Vec<PathBuf>,
    pub(crate) ignore_patterns: Vec<String>,
    pub(crate) ignore_paths: Vec<PathBuf>,
    pub(crate) tick_ms: u64,
    pub(crate) ignore_worker_failure: bool,
}

pub(crate) fn run_reload_supervisor(
    executable: &str,
    argv: &[String],
    config: ReloadConfig,
) -> Result<(), String> {
    let ignore_globs = build_reload_ignore_globs(&config)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            let _ = tx.send(event);
        },
        NotifyConfig::default(),
    )
    .map_err(|err| err.to_string())?;

    for dir in &config.watch_dirs {
        watcher
            .watch(dir, RecursiveMode::Recursive)
            .map_err(|err| err.to_string())?;
    }

    let mut child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;
    let debounce = Duration::from_millis(config.tick_ms);

    loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            if config.ignore_worker_failure {
                eprintln!("FastrAPI reload: child exited with status {status}; restarting");
                child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;
                continue;
            }
            return Err(format!("reload child exited with status {status}"));
        }

        match rx.recv_timeout(debounce) {
            Ok(Ok(event)) => {
                if !reload_event_matches(&event, &config, &ignore_globs) {
                    continue;
                }
                println!("");
                println!("Python file change detected, restarting server...");
                stop_child(&mut child);
                child = spawn_reload_child(executable, argv).map_err(|err| err.to_string())?;
                while rx.try_recv().is_ok() {}
            }
            Ok(Err(err)) => return Err(err.to_string()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("reload watcher stopped".to_string());
            }
        }
    }
}
pub(crate) fn spawn_reload_child(executable: &str, argv: &[String]) -> std::io::Result<Child> {
    let mut command = Command::new(executable);
    command
        .args(argv)
        .env("FASTRAPI_RELOAD_CHILD", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command.spawn()
}

pub(crate) fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn reload_event_matches(
    event: &notify::Event,
    config: &ReloadConfig,
    ignore_globs: &Option<GlobSet>,
) -> bool {
    event.paths.iter().any(|path| {
        path.extension().and_then(|ext| ext.to_str()) == Some("py")
            && !is_reload_ignored(path, config, ignore_globs)
    })
}

pub(crate) fn is_reload_ignored(
    path: &Path,
    config: &ReloadConfig,
    ignore_globs: &Option<GlobSet>,
) -> bool {
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

    ignore_globs
        .as_ref()
        .is_some_and(|globs| globs.is_match(path))
}

pub(crate) fn is_default_reload_ignored_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | ".venv" | "__pycache__" | "target"))
}

pub(crate) fn build_reload_ignore_globs(config: &ReloadConfig) -> Result<Option<GlobSet>, String> {
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

pub(crate) fn resolve_reload_dirs(
    script_path: &str,
    reload_dirs: Option<Vec<String>>,
) -> Vec<PathBuf> {
    if let Some(dirs) = reload_dirs {
        return dirs.into_iter().map(PathBuf::from).collect();
    }

    let script = PathBuf::from(script_path);
    if let Some(parent) = script.parent() {
        if !parent.as_os_str().is_empty() {
            return vec![parent.to_path_buf()];
        }
    }

    std::env::current_dir()
        .map(|dir| vec![dir])
        .unwrap_or_else(|_| vec![PathBuf::from(".")])
}
