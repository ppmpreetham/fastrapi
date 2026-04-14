import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

import httpx


ROOT = Path(__file__).resolve().parents[1]


def get_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for_server(
    proc: subprocess.Popen[str], port: int, path: str = "/", timeout: float = 15.0
):
    deadline = time.time() + timeout
    last_error = None

    while time.time() < deadline:
        if proc.poll() is not None:
            output = proc.stdout.read() if proc.stdout is not None else ""
            raise AssertionError(
                f"server exited early with code {proc.returncode}\n{output}"
            )

        try:
            response = httpx.get(f"http://127.0.0.1:{port}{path}", timeout=0.5)
            return response
        except Exception as exc:  # pragma: no cover - retry loop
            last_error = exc
            time.sleep(0.1)

    output = proc.stdout.read() if proc.stdout is not None else ""
    raise AssertionError(f"server did not start in time: {last_error}\n{output}")


def stop_process(proc: subprocess.Popen[str], timeout: float = 10.0) -> str:
    proc.send_signal(signal.SIGINT)

    try:
        output, _ = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        output, _ = proc.communicate(timeout=5.0)
        raise AssertionError(f"server did not stop cleanly\n{output}")

    if proc.returncode not in (0, -signal.SIGINT):
        raise AssertionError(f"unexpected exit code {proc.returncode}\n{output}")

    return output


def write_app_script(path: Path, contents: str) -> None:
    path.write_text(contents, encoding="utf-8")


def test_lifespan_takes_precedence_and_receives_app(tmp_path: Path):
    port = get_free_port()
    events_file = tmp_path / "lifespan-events.txt"
    script_file = tmp_path / "lifespan_app.py"

    write_app_script(
        script_file,
        f'''
from contextlib import asynccontextmanager
from pathlib import Path

from fastrapi import FastrAPI


EVENTS = Path(r"{events_file}")


def log_event(name: str) -> None:
    with EVENTS.open("a", encoding="utf-8") as handle:
        handle.write(name + "\\n")


def legacy_startup() -> None:
    log_event("legacy_startup")


def legacy_shutdown() -> None:
    log_event("legacy_shutdown")


@asynccontextmanager
async def lifespan(app: FastrAPI):
    log_event("lifespan_enter")
    app.title = "started"
    try:
        yield
    finally:
        log_event("lifespan_exit")


app = FastrAPI(
    title="before",
    lifespan=lifespan,
    on_startup=[legacy_startup],
    on_shutdown=[legacy_shutdown],
)


@app.get("/state")
def state():
    events = EVENTS.read_text(encoding="utf-8").splitlines() if EVENTS.exists() else []
    return {{"title": app.title, "events": events}}


app.serve(host="127.0.0.1", port={port})
''',
    )

    proc = subprocess.Popen(
        [sys.executable, str(script_file)],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    try:
        response = wait_for_server(proc, port, "/state")
        assert response.status_code == 200
        assert response.json() == {
            "title": "started",
            "events": ["lifespan_enter"],
        }

        stop_process(proc)

        assert events_file.read_text(encoding="utf-8").splitlines() == [
            "lifespan_enter",
            "lifespan_exit",
        ]
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.communicate(timeout=5.0)


def test_startup_and_shutdown_support_sync_and_async_handlers(tmp_path: Path):
    port = get_free_port()
    events_file = tmp_path / "startup-shutdown-events.txt"
    script_file = tmp_path / "startup_shutdown_app.py"

    write_app_script(
        script_file,
        f'''
from pathlib import Path

from fastrapi import FastrAPI


EVENTS = Path(r"{events_file}")


def log_event(name: str) -> None:
    with EVENTS.open("a", encoding="utf-8") as handle:
        handle.write(name + "\\n")


def startup_sync() -> None:
    log_event("startup_sync")


async def startup_async() -> None:
    log_event("startup_async")


def shutdown_sync() -> None:
    log_event("shutdown_sync")


async def shutdown_async() -> None:
    log_event("shutdown_async")


app = FastrAPI(
    on_startup=[startup_sync, startup_async],
    on_shutdown=[shutdown_async, shutdown_sync],
)


@app.get("/events")
def events():
    return EVENTS.read_text(encoding="utf-8").splitlines()


app.serve(host="127.0.0.1", port={port})
''',
    )

    proc = subprocess.Popen(
        [sys.executable, str(script_file)],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    try:
        response = wait_for_server(proc, port, "/events")
        assert response.status_code == 200
        assert response.json() == ["startup_sync", "startup_async"]

        stop_process(proc)

        assert events_file.read_text(encoding="utf-8").splitlines() == [
            "startup_sync",
            "startup_async",
            "shutdown_async",
            "shutdown_sync",
        ]
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.communicate(timeout=5.0)
