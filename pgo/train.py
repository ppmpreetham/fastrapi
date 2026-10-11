import json
import os
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen


HOST = "127.0.0.1"
PORT = 8000
BASE = f"http://{HOST}:{PORT}"
DEFAULT_ITERATIONS = 200
DEFAULT_MAX_SECONDS = 90
PAYLOAD_DIR = Path(__file__).resolve().parent / "payloads"
EXPECTED_ERROR_STATUSES = {400, 401, 403, 404, 405, 422}


def ensure_training_dependencies():
    try:
        import pydantic  # noqa: F401
    except ModuleNotFoundError as exc:
        raise RuntimeError(
            "PGO training requires the wheel dependencies; install the project "
            "before invoking the trainer"
        ) from exc


def load_payloads():
    payloads = {}
    for path in sorted(PAYLOAD_DIR.glob("*.json")):
        with path.open("r", encoding="utf-8") as handle:
            payloads[path.stem] = json.load(handle)
    return payloads


def wait_for_port(host, port, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("server did not start")


def request(path, method="GET", body=None, headers=None, expected=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode("utf-8")
    request_headers = dict(headers or {})

    if body is not None:
        request_headers.setdefault("Content-Type", "application/json")
    request_headers.setdefault("Accept", "application/json, text/html;q=0.9, */*;q=0.8")
    request_headers.setdefault("User-Agent", "fastrapi-pgo-trainer/1.0")

    req = Request(url, data=data, method=method, headers=request_headers)
    try:
        with urlopen(req, timeout=10) as resp:
            resp.read()
            return resp.status
    except HTTPError as exc:
        if expected and exc.code in expected:
            exc.read()
            return exc.code
        raise


def query(path, params):
    return f"{path}?{urlencode(params)}"


def data_body(payload):
    return {"data": payload}


def run_static_routes():
    request("/")
    request("/health")
    request("/status")
    request("/config")
    request("/version")
    request("/html")
    request("/static/")
    request("/download")
    request("/heavy", headers={"Accept-Encoding": "gzip"})
    request("/docs", headers={"Accept": "text/html"})
    request("/api-docs/openapi.json")
    request("/status", "HEAD")
    request("/status", "OPTIONS")
    request(
        "/auth/refresh",
        "OPTIONS",
        headers={
            "Origin": "http://example.com",
            "Access-Control-Request-Method": "POST",
            "Access-Control-Request-Headers": "content-type,token",
        },
    )


def run_read_routes(index):
    user_id = [1, 2, 42][index % 3]
    sku = "sku-1001" if index % 2 == 0 else "sku-2002"

    request(f"/users/{user_id}")
    request(f"/users/{user_id}/orders/ord-{1000 + index % 50}")
    request(f"/products/{sku}")
    request(f"/orders/ord-{1000 + index % 50}")
    request(f"/inventory/{sku}")
    request("/protected")
    request("/dependency-user")
    request("/auth/header", headers={"token": "valid-token"})
    request("/admin", headers={"token": "admin-token"})
    request("/session", headers={"Cookie": "fastrapi_session=pgo-cookie-value; theme=dark"})
    request(query("/validate/query", {"age": 34, "name": "Jordan"}))
    request("/validate/path/500")
    request(
        query(
            "/search",
            {
                "q": "api gateway" if index % 2 == 0 else "observability",
                "limit": 10 + (index % 5),
                "offset": index % 25,
            },
        )
    )


def run_write_routes(payloads, index):
    request("/echo", "POST", data_body(payloads["small"]))
    request("/upload-json", "POST", data_body(payloads["large"]))
    request("/users", "POST", payloads["user"])
    request(
        "/register",
        "POST",
        {"user": payloads["user"], "address": payloads["address"]},
    )
    request(f"/users/{(index % 3) + 1}", "PATCH", data_body(payloads["profile_patch"]))
    request("/auth/login", "POST", payloads["login"])
    request(
        "/auth/refresh",
        "POST",
        data_body({"scope": "refresh", "issued_at": "2026-05-11T10:00:00Z"}),
        headers={"token": "valid-token"},
    )
    request(
        "/admin/audit",
        "POST",
        data_body(payloads["audit_events"]),
        headers={"token": "admin-token"},
    )
    request("/products", "POST", payloads["product"])
    request("/products/sku-1001", "PUT", payloads["product"])
    request("/orders", "POST", payloads["order"])
    request(f"/orders/ord-{1000 + index % 50}", "PUT", payloads["order"])
    request(f"/orders/ord-{1000 + index % 50}", "PATCH", data_body(payloads["profile_patch"]))
    request(f"/orders/ord-{1000 + index % 50}", "DELETE")
    request("/cart", "POST", data_body(payloads["cart"]))
    request("/inventory/bulk", "POST", data_body(payloads["inventory_bulk"]))
    request("/events", "POST", payloads["analytics_batch"])
    request("/metrics", "POST", data_body(payloads["metrics"]))
    request("/audit", "POST", data_body(payloads["audit_events"]))
    request("/comments", "POST", data_body(payloads["comments"]))
    request("/support/tickets", "POST", payloads["support_ticket"])
    request("/notifications", "POST", data_body(payloads["notification_batch"]))
    request("/middleware/probe", "POST", data_body(payloads["small"]))
    request("/echo", "POST", data_body(payloads["organization"]))
    request("/echo", "POST", data_body(payloads["search_filters"]))


def run_expected_errors():
    request("/missing-route", expected=EXPECTED_ERROR_STATUSES)
    request("/", "POST", expected=EXPECTED_ERROR_STATUSES)
    request("/users/not-an-int", expected=EXPECTED_ERROR_STATUSES)
    request("/auth/header", expected=EXPECTED_ERROR_STATUSES)
    request("/admin", headers={"token": "valid-token"}, expected=EXPECTED_ERROR_STATUSES)
    request(
        "/middleware/probe",
        "POST",
        data_body({"blocked": True}),
        headers={"x-pgo-block": "1"},
        expected=EXPECTED_ERROR_STATUSES,
    )


def run_websocket(index):
    """Exercise the upgrade, frame parsing, and close paths without a client dependency."""

    def recv_exact(sock, size):
        data = bytearray()
        while len(data) < size:
            chunk = sock.recv(size - len(data))
            if not chunk:
                raise RuntimeError("websocket closed before response frame completed")
            data.extend(chunk)
        return bytes(data)

    with socket.create_connection((HOST, PORT), timeout=5) as sock:
        sock.settimeout(5)
        sock.sendall(
            (
                "GET /ws HTTP/1.1\r\n"
                f"Host: {HOST}:{PORT}\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                "Sec-WebSocket-Key: cGdvLXRyYWluaW5nLWtleQ==\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        response = bytearray()
        while b"\r\n\r\n" not in response:
            chunk = sock.recv(4096)
            if not chunk:
                raise RuntimeError("websocket closed during upgrade")
            response.extend(chunk)
        if b" 101 " not in response:
            raise RuntimeError("websocket upgrade failed")
        payload = f"pgo-{index}".encode()
        mask = b"PGO!"
        frame = bytes([0x81, 0x80 | len(payload)]) + mask
        frame += bytes(byte ^ mask[offset % 4] for offset, byte in enumerate(payload))
        sock.sendall(frame)
        header = recv_exact(sock, 2)
        if len(header) != 2 or header[0] & 0x0F != 1:
            raise RuntimeError("websocket response frame missing")
        length = header[1] & 0x7F
        if length == 126:
            length = struct.unpack("!H", recv_exact(sock, 2))[0]
        body = recv_exact(sock, length)
        if body != b"echo:" + payload:
            raise RuntimeError("unexpected websocket response")


def run_workload(iterations, max_seconds):
    payloads = load_payloads()
    started = time.time()

    print(
        f"PGO workload starting: iterations={iterations}, "
        f"max_seconds={max_seconds or 'disabled'}",
        flush=True,
    )

    for index in range(iterations):
        if max_seconds and time.time() - started >= max_seconds:
            print(f"PGO workload time budget reached after {index} iterations", flush=True)
            break

        run_static_routes()
        run_read_routes(index)
        run_write_routes(payloads, index)

        if index % 25 == 0:
            run_expected_errors()
            run_websocket(index)

        completed = index + 1
        if completed == 1 or completed % 25 == 0 or completed == iterations:
            elapsed = time.time() - started
            print(f"PGO workload progress: {completed}/{iterations} in {elapsed:.1f}s", flush=True)


def main():
    ensure_training_dependencies()
    iterations = max(1, min(int(os.environ.get("PGO_ITERATIONS", DEFAULT_ITERATIONS)), 1000))
    max_seconds = max(1, min(int(os.environ.get("PGO_MAX_SECONDS", DEFAULT_MAX_SECONDS)), 600))
    proc = subprocess.Popen([sys.executable, "-m", "pgo.app"])

    try:
        wait_for_port(HOST, PORT)
        run_workload(iterations, max_seconds)
    except URLError:
        proc.terminate()
        raise
    finally:
        if proc.poll() is None:
            proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


if __name__ == "__main__":
    main()
