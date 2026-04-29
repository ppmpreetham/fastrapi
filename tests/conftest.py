import asyncio
import signal
import socket
import subprocess
import sys
import textwrap
import time
from pathlib import Path

import httpx
import pytest


ROOT = Path(__file__).resolve().parents[1]


def run_async(async_fn):
    return asyncio.run(async_fn())


@pytest.fixture
def await_result():
    return run_async


def get_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for_server(
    proc: subprocess.Popen[str],
    port: int,
    path: str = "/health",
    timeout: float = 15.0,
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
            return httpx.get(f"http://127.0.0.1:{port}{path}", timeout=0.5)
        except Exception as exc:
            last_error = exc
            time.sleep(0.1)

    output = proc.stdout.read() if proc.stdout is not None else ""
    raise AssertionError(f"server did not start in time: {last_error}\n{output}")


def stop_process(proc: subprocess.Popen[str], timeout: float = 10.0) -> str:
    if sys.platform == "win32":
        proc.send_signal(signal.SIGTERM)
        valid_exit_codes = (0, 1, 15, signal.SIGTERM)
    else:
        proc.send_signal(signal.SIGINT)
        valid_exit_codes = (0, -signal.SIGINT)

    try:
        output, _ = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        output, _ = proc.communicate(timeout=5.0)
        raise AssertionError(f"server did not stop cleanly\n{output}")

    if proc.returncode not in valid_exit_codes:
        raise AssertionError(f"unexpected exit code {proc.returncode}\n{output}")

    return output


@pytest.fixture
def run_server(tmp_path: Path):
    processes: list[subprocess.Popen[str]] = []

    def start(script: str, *, health_path: str = "/health") -> str:
        port = get_free_port()
        script_file = tmp_path / f"app_{len(processes)}.py"
        script_file.write_text(
            textwrap.dedent(script).format(port=port),
            encoding="utf-8",
        )

        proc = subprocess.Popen(
            [sys.executable, str(script_file)],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        processes.append(proc)
        wait_for_server(proc, port, health_path)
        return f"http://127.0.0.1:{port}"

    yield start

    for proc in processes:
        if proc.poll() is None:
            stop_process(proc)
