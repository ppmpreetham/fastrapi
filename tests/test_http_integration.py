import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

import httpx
import pytest


ROOT = Path(__file__).resolve().parents[1]


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
            response = httpx.get(f"http://127.0.0.1:{port}{path}", timeout=0.5)
            return response
        except Exception as exc:  # pragma: no cover - retry loop
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


def write_app_script(path: Path, contents: str) -> None:
    path.write_text(contents, encoding="utf-8")


@pytest.fixture(scope="module")
def server(tmp_path_factory: pytest.TempPathFactory) -> str:
    port = get_free_port()
    tmp_path = tmp_path_factory.mktemp("http-integration")
    script_file = tmp_path / "http_integration_app.py"

    write_app_script(
        script_file,
        f"""
from fastrapi import FastrAPI, Query, Header, Cookie
from fastrapi.responses import RedirectResponse
from fastrapi.middleware import CORSMiddleware, GZipMiddleware, TrustedHostMiddleware

app = FastrAPI()

app.add_middleware(
    TrustedHostMiddleware,
    allowed_hosts=["127.0.0.1", "127.0.0.1:{port}", "good.test"],
    www_redirect=True,
)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["https://example.com"],
    allow_methods=["GET", "OPTIONS"],
    allow_headers=["*"],
    allow_credentials=False,
)

app.add_middleware(GZipMiddleware, minimum_size=10, compresslevel=6)


@app.get("/health")
def health():
    return {{"status": "ok"}}


@app.get("/cors")
def cors_route():
    return {{"cors": True}}


@app.get("/gzip")
def gzip_route():
    return {{"payload": "x" * 200}}


@app.get("/headers")
def headers_route(
    x_token: str = Header(...),
    alias_token: str = Header(..., alias="X-Alias"),
    x_custom: str = Header(...),
):
    return {{
        "x_token": x_token,
        "alias_token": alias_token,
        "x_custom": x_custom,
    }}


@app.get("/headers/validate")
def headers_validate(token: str = Header(..., alias="X-Short", min_length=3)):
    return {{"token": token}}


@app.get("/cookies")
def cookies_route(
    session_id: str = Cookie(...),
    user_id: str = Cookie(..., alias="user-id"),
):
    return {{"session_id": session_id, "user_id": user_id}}


@app.get("/query")
def query_route(flag: bool = Query(...), count: int = Query(..., ge=1)):
    return {{"flag": flag, "count": count}}


@app.get("/auto-list")
def auto_list():
    return [1, 2, 3]


@app.get("/auto-int")
def auto_int():
    return 7


@app.get("/auto-none")
def auto_none():
    return None


@app.get("/redirect-perm")
def redirect_perm() -> RedirectResponse:
    return RedirectResponse("/target", status_code=301)


@app.get("/redirect-temp")
def redirect_temp() -> RedirectResponse:
    return RedirectResponse("/target", status_code=307)




app.serve(host="127.0.0.1", port={port})
""",
    )

    proc = subprocess.Popen(
        [sys.executable, str(script_file)],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    try:
        wait_for_server(proc, port, "/health")
        yield f"http://127.0.0.1:{port}"
    finally:
        if proc.poll() is None:
            stop_process(proc)


def test_cors_headers(server: str):
    response = httpx.get(
        f"{server}/cors",
        headers={"Origin": "https://example.com"},
    )
    assert response.status_code == 200
    assert response.headers.get("access-control-allow-origin") == "https://example.com"


def test_cors_preflight(server: str):
    response = httpx.options(
        f"{server}/cors",
        headers={
            "Origin": "https://example.com",
            "Access-Control-Request-Method": "GET",
        },
    )
    assert response.status_code in {200, 204}
    assert response.headers.get("access-control-allow-origin") == "https://example.com"


def test_gzip_compression(server: str):
    response = httpx.get(
        f"{server}/gzip",
        headers={"accept-encoding": "gzip"},
    )
    assert response.status_code == 200
    assert "gzip" in response.headers.get("content-encoding", "")


def test_trusted_host_allows_configured_host(server: str):
    response = httpx.get(
        f"{server}/health",
        headers={"host": "good.test"},
    )
    assert response.status_code == 200


def test_trusted_host_rejects_unknown_host(server: str):
    response = httpx.get(
        f"{server}/health",
        headers={"host": "bad.test"},
    )
    assert response.status_code == 400
    assert "Invalid Host" in response.text


def test_trusted_host_www_redirect(server: str):
    response = httpx.get(
        f"{server}/health",
        headers={"host": "www.good.test"},
    )
    assert response.status_code == 301


def test_header_parsing_with_alias_and_underscores(server: str):
    response = httpx.get(
        f"{server}/headers",
        headers={
            "x-token": "alpha",
            "X-Alias": "bravo",
            "x-custom": "charlie",
        },
    )
    assert response.status_code == 200
    assert response.json() == {
        "x_token": "alpha",
        "alias_token": "bravo",
        "x_custom": "charlie",
    }


def test_header_constraints_are_enforced(server: str):
    response = httpx.get(
        f"{server}/headers/validate",
        headers={"X-Short": "ab"},
    )
    assert response.status_code == 422
    assert "detail" in response.json()


def test_cookie_parsing_with_alias(server: str):
    response = httpx.get(
        f"{server}/cookies",
        cookies={"session_id": "abc123", "user-id": "42"},
    )
    assert response.status_code == 200
    assert response.json() == {"session_id": "abc123", "user_id": "42"}


def test_query_scalar_parsing(server: str):
    response = httpx.get(f"{server}/query?flag=true&count=2")
    assert response.status_code == 200
    assert response.json() == {"flag": True, "count": 2}


def test_query_scalar_validation_error(server: str):
    response = httpx.get(f"{server}/query?flag=maybe&count=2")
    assert response.status_code == 422
    assert "detail" in response.json()


def test_auto_response_list_and_scalar(server: str):
    response = httpx.get(f"{server}/auto-list")
    assert response.status_code == 200
    assert response.json() == [1, 2, 3]

    response = httpx.get(f"{server}/auto-int")
    assert response.status_code == 200
    assert response.json() == 7


def test_auto_response_none(server: str):
    response = httpx.get(f"{server}/auto-none")
    assert response.status_code == 204


def test_redirect_response_status_codes(server: str):
    response = httpx.get(f"{server}/redirect-perm", follow_redirects=False)
    assert response.status_code == 308
    assert response.headers.get("location") == "/target"

    response = httpx.get(f"{server}/redirect-temp", follow_redirects=False)
    assert response.status_code == 307
    assert response.headers.get("location") == "/target"
