"""Security schemes, websocket DI/features, OpenAPI security, ordering."""
import base64
import json
import os
import socket
import struct
import threading
import time

from fastrapi import FastrAPI, Depends
from fastrapi.responses import JSONResponse
from fastrapi.security import (
    APIKeyHeader,
    HTTPBasic,
    OAuth2PasswordBearer,
    OpenIdConnect,
)


def _free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


class LiveServer:
    """Starts an app on a free port and speaks raw HTTP / WebSocket."""

    def __init__(self, app: FastrAPI):
        self.port = _free_port()
        threading.Thread(
            target=lambda: app.serve(host="127.0.0.1", port=self.port),
            daemon=True,
        ).start()
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                with socket.create_connection(("127.0.0.1", self.port), timeout=0.4) as s:
                    probe = (
                        "GET /openapi.json HTTP/1.1\r\n"
                        f"Host: 127.0.0.1:{self.port}\r\nConnection: close\r\n\r\n"
                    ).encode()
                    s.sendall(probe)
                    if b" 200 " in s.recv(512):
                        return
            except OSError:
                pass
            time.sleep(0.05)
        raise RuntimeError("server never became ready")

    def http(self, method: str, path: str, headers=None):
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as s:
            # HTTP/1.0 -> connection-close delimiting, no chunked encoding.
            head = f"{method} {path} HTTP/1.0\r\nHost: x\r\n"
            for k, v in (headers or {}).items():
                head += f"{k}: {v}\r\n"
            s.sendall((head + "\r\n").encode())
            raw = b""
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break
                raw += chunk
        head_part, _, body = raw.partition(b"\r\n\r\n")
        status = int(head_part.split(b" ")[1])
        body_str = body.decode(errors="replace")
        try:
            parsed = json.loads(body_str)
        except ValueError:
            parsed = body_str
        return status, parsed

    def http_headers(self, method: str, path: str, headers=None):
        """Like http(), but also returns the response headers (lowercased)."""
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as s:
            head = f"{method} {path} HTTP/1.0\r\nHost: x\r\n"
            for k, v in (headers or {}).items():
                head += f"{k}: {v}\r\n"
            s.sendall((head + "\r\n").encode())
            raw = b""
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break
                raw += chunk
        head_part, _, body = raw.partition(b"\r\n\r\n")
        status = int(head_part.split(b" ")[1])
        resp_headers = {}
        for line in head_part.split(b"\r\n")[1:]:
            name, sep, value = line.partition(b":")
            if sep:
                resp_headers[name.decode().strip().lower()] = value.decode().strip()
        try:
            parsed = json.loads(body.decode(errors="replace"))
        except ValueError:
            parsed = body.decode(errors="replace")
        return status, resp_headers, parsed

    def openapi(self):
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as s:
            s.sendall(
                b"GET /openapi.json HTTP/1.0\r\nHost: x\r\n\r\n"
            )
            raw = b""
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break
                raw += chunk
        return json.loads(raw.partition(b"\r\n\r\n")[2].decode())

    def websocket(self, path: str):
        key = base64.b64encode(os.urandom(16)).decode()
        sock = socket.create_connection(("127.0.0.1", self.port), timeout=5)
        sock.sendall(
            (
                f"GET {path} HTTP/1.1\r\n"
                f"Host: 127.0.0.1:{self.port}\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        response = sock.recv(4096)
        assert b" 101 " in response
        return sock


def _send_text(sock: socket.socket, text: str) -> None:
    payload = text.encode()
    mask = os.urandom(4)
    header = bytearray([0x81, 0x80 | len(payload)])
    masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    sock.sendall(bytes(header) + mask + masked)


def _recv_frame(sock: socket.socket) -> tuple[int, bytes]:
    first = sock.recv(2)
    assert len(first) == 2
    opcode = first[0] & 0x0F
    length = first[1] & 0x7F
    if length == 126:
        length = struct.unpack("!H", sock.recv(2))[0]
    payload = bytearray()
    while len(payload) < length:
        payload.extend(sock.recv(length - len(payload)))
    return opcode, bytes(payload)


# ---------------------------------------------------------------- security


def test_oauth2_bearer_flow():
    oauth2 = OAuth2PasswordBearer(token_url="/token")
    app = FastrAPI()

    @app.get("/me")
    def me(token=Depends(oauth2)):
        return {"token": token}

    live = LiveServer(app)

    status, body = live.http("GET", "/me")
    assert status == 401
    assert body == {"detail": "Not authenticated"}

    status, body = live.http("GET", "/me", {"Authorization": "Bearer tok123"})
    assert status == 200
    assert body == {"token": "tok123"}


def test_http_basic_and_api_key():
    basic = HTTPBasic()
    api_key = APIKeyHeader(name="X-Key")
    app = FastrAPI()

    @app.get("/basic")
    def basic_route(creds=Depends(basic)):
        return {"user": creds.username}

    @app.get("/keyed")
    def keyed(key=Depends(api_key)):
        return {"key": key}

    live = LiveServer(app)

    encoded = base64.b64encode(b"alice:s3cret").decode()
    status, body = live.http("GET", "/basic", {"Authorization": f"Basic {encoded}"})
    assert (status, body) == (200, {"user": "alice"})

    status, _ = live.http("GET", "/basic")
    assert status == 401

    status, body = live.http("GET", "/keyed", {"X-Key": "abc"})
    assert (status, body) == (200, {"key": "abc"})


def test_unauthorized_carries_www_authenticate_challenge():
    """RFC 9110 requires a 401 to carry a WWW-Authenticate challenge.

    Regression guard: `HTTPException`'s `headers` argument is positional, so
    building it with `HTTPException(status, detail, **headers)` turns each
    header name into a keyword argument. `HTTPException.__new__` rejects those,
    and the caller gets a 500 instead of a 401 with a challenge.
    """
    from fastrapi.security import HTTPBearer

    bearer = HTTPBearer()
    basic_realm = HTTPBasic(realm="myrealm")
    api_key = APIKeyHeader(name="X-Key")
    app = FastrAPI()

    @app.get("/bearer")
    def bearer_route(creds=Depends(bearer)):
        return {}

    @app.get("/basic-realm")
    def basic_realm_route(creds=Depends(basic_realm)):
        return {}

    @app.get("/keyed")
    def keyed_route(key=Depends(api_key)):
        return {}

    live = LiveServer(app)

    status, headers, _ = live.http_headers("GET", "/bearer")
    assert status == 401
    assert headers.get("www-authenticate") == "Bearer"

    # fastapi sends a bare `Basic` when no realm is set, and quotes the realm when it is.
    status, headers, _ = live.http_headers("GET", "/basic-realm")
    assert status == 401
    assert headers.get("www-authenticate") == 'Basic realm="myrealm"'

    # api keys are non-standard but fastapi still answers 401 with an `APIKey` challenge.
    status, headers, _ = live.http_headers("GET", "/keyed")
    assert status == 401
    assert headers.get("www-authenticate") == "APIKey"


# ---------------------------------------------------------------- openapi


def test_openapi_security_schemes_and_requirements():
    from fastapi import HTTPException as _HE  # noqa: F401

    oauth2 = OAuth2PasswordBearer("/token", scopes={"read": "Read"})
    oidc = OpenIdConnect("https://id.example/.well-known")
    app = FastrAPI()

    @app.get("/a")
    def a(v=Depends(oauth2)):
        return {}

    @app.get("/b")
    def b(v=Depends(oauth2), w=Depends(oidc)):
        return {}

    live = LiveServer(app)
    spec = live.openapi()

    schemes = spec["components"]["securitySchemes"]
    assert set(schemes) == {"OAuth2", "OpenIdConnect"}
    assert schemes["OAuth2"]["type"] == "oauth2"
    assert schemes["OAuth2"]["flows"]["password"]["scopes"] == {"read": "Read"}
    assert schemes["OpenIdConnect"]["type"] == "openIdConnect"

    assert spec["paths"]["/a"]["get"]["security"] == [{"OAuth2": []}]
    assert set(spec["paths"]["/b"]["get"]["security"][0]) == {"OAuth2", "OpenIdConnect"}


# ------------------------------------------------------------- websockets


def test_websocket_dependency_injection_and_json():
    app = FastrAPI()

    def room_dep(room: str = "", token: str = ""):
        return f"{room}:{token}"

    @app.websocket("/ws/{room}")
    async def handler(websocket, dep=Depends(room_dep)):
        await websocket.accept()
        await websocket.send_json({"dep": dep})
        async for message in websocket.iter_text():
            await websocket.send_text(message.upper())
            if message == "bye":
                break
        await websocket.close(1000)

    live = LiveServer(app)
    sock = live.websocket("/ws/lobby?token=tok")

    opcode, payload = _recv_frame(sock)
    assert opcode == 1
    assert json.loads(payload) == {"dep": "lobby:tok"}

    _send_text(sock, "hi")
    opcode, payload = _recv_frame(sock)
    assert payload.decode() == "HI"

    _send_text(sock, "bye")
    opcode, payload = _recv_frame(sock)
    assert payload.decode() == "BYE"

    # iterator exhausted -> server closes with our code
    opcode, payload = _recv_frame(sock)
    assert opcode == 8  # close frame
    assert struct.unpack("!H", payload[:2])[0] == 1000


# ---------------------------------------------------------------- ordering


def test_middleware_ordering_last_declared_outermost():
    import json as _json

    class Tagger:
        """Prepends its tag to the JSON body's `seen` list."""

        def __init__(self, tag):
            self.tag = tag

        async def __call__(self, request, call_next):
            response = await call_next(request)
            try:
                seen = _json.loads(response.body).get("seen", [])
            except (ValueError, AttributeError, TypeError):
                seen = []
            seen.insert(0, self.tag)
            return JSONResponse({"seen": seen})

    app = FastrAPI()

    @app.get("/")
    def index():
        return {"seen": []}

    # NOTE: fastrapi applies declared middlewares reversed (Starlette-like):
    # the LAST add_middleware wraps OUTERMOST, so it runs FIRST.
    app.add_middleware(Tagger, tag="inner")
    app.add_middleware(Tagger, tag="outer")

    live = LiveServer(app)
    status, body = live.http("GET", "/")

    assert status == 200
    # outer middleware observes first; inner second.
    assert body["seen"] == ["outer", "inner"]
