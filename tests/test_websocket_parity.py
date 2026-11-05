import base64
import json
import os
import socket
import struct

from test_websocket_smoke import _free_port, _recv_frame, _serve

from fastrapi import FastrAPI


def _connect(port, path, headers=(), timeout=5):
    key = base64.b64encode(os.urandom(16)).decode()
    sock = socket.create_connection(("127.0.0.1", port), timeout=timeout)
    extra = "".join(f"{line}\r\n" for line in headers)
    sock.sendall(
        (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: 127.0.0.1:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            f"{extra}\r\n"
        ).encode()
    )
    response = b""
    while b"\r\n\r\n" not in response:
        chunk = sock.recv(4096)
        if not chunk:
            break
        response += chunk
    head, _, rest = response.partition(b"\r\n\r\n")
    status = int(head.split(b" ")[1])
    return status, head.decode(), rest, sock


def _send(sock, payload, opcode=0x1):
    data = payload.encode() if isinstance(payload, str) else payload
    mask = os.urandom(4)
    masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(data))
    if len(data) < 126:
        header = bytes([0x80 | opcode, 0x80 | len(data)])
    else:
        header = bytes([0x80 | opcode, 0x80 | 126]) + struct.pack("!H", len(data))
    sock.sendall(header + mask + masked)


def _close_code(payload):
    return struct.unpack("!H", payload[:2])[0], payload[2:].decode()


def _app_with_port(app):
    port = _free_port()
    _serve(app, port)
    return port


def test_websocket_connection_attributes():
    app = FastrAPI()

    @app.get("/items/{item_id}", name="item")
    def item(item_id: int):
        return {}

    @app.websocket("/ws/{room}")
    async def ws(websocket):
        await websocket.accept()
        await websocket.send_json(
            {
                "url": str(websocket.url),
                "base_url": str(websocket.base_url),
                "path_params": websocket.path_params,
                "cookies": websocket.cookies,
                "client": websocket.client.host,
                "has_app": websocket.app is not None,
                "url_for": str(websocket.url_for("item", item_id=1)),
                "query": websocket.query_params["q"],
                "header": websocket.headers["x-token"],
            }
        )
        await websocket.close()

    port = _app_with_port(app)
    status, _, _, sock = _connect(port, "/ws/lobby?q=1", ["X-Token: t", "Cookie: session=abc"])
    assert status == 101
    opcode, payload = _recv_frame(sock)
    data = json.loads(payload)
    assert data == {
        "url": f"ws://127.0.0.1:{port}/ws/lobby?q=1",
        "base_url": f"ws://127.0.0.1:{port}/",
        "path_params": {"room": "lobby"},
        "cookies": {"session": "abc"},
        "client": "127.0.0.1",
        "has_app": True,
        "url_for": f"http://127.0.0.1:{port}/items/1",
        "query": "1",
        "header": "t",
    }


def test_websocket_typed_params_and_keyword_websocket():
    from typing import Annotated

    from fastrapi import Cookie, Header, WebSocket

    app = FastrAPI()

    @app.websocket("/items/{item_id}")
    async def ws(
        item_id: int,
        q: int,
        token: Annotated[str, Header()],
        session: Annotated[str | None, Cookie()] = None,
        *,
        websocket: WebSocket,
    ):
        await websocket.accept()
        await websocket.send_json({"item_id": item_id, "q": q, "token": token, "session": session})
        await websocket.close()

    port = _app_with_port(app)
    status, _, _, sock = _connect(port, "/items/5?q=2", ["Token: abc", "Cookie: session=s1"])
    assert status == 101
    assert json.loads(_recv_frame(sock)[1]) == {"item_id": 5, "q": 2, "token": "abc", "session": "s1"}
    status, _, _, _ = _connect(port, "/items/five?q=2", ["Token: abc"])
    assert status == 403


def test_websocket_dependency_receives_websocket():
    from fastrapi import Depends, WebSocket

    app = FastrAPI()

    def client_path(websocket: WebSocket):
        return websocket.url.path

    @app.websocket("/dep")
    async def ws(websocket: WebSocket, path: str = Depends(client_path)):
        await websocket.accept()
        await websocket.send_text(path)
        await websocket.close()

    port = _app_with_port(app)
    status, _, _, sock = _connect(port, "/dep")
    assert status == 101
    assert _recv_frame(sock) == (0x1, b"/dep")


def test_websocket_slow_accept_is_not_timed_out():
    import asyncio

    app = FastrAPI()

    @app.websocket("/slow")
    async def ws(websocket):
        await asyncio.sleep(11)
        await websocket.accept()
        await websocket.send_text("late")
        await websocket.close()

    port = _app_with_port(app)
    status, _, _, sock = _connect(port, "/slow", timeout=15)
    assert status == 101
    assert _recv_frame(sock) == (0x1, b"late")


def test_websocket_errors_map_to_close_codes():
    from fastrapi import Depends, WebSocket, WebSocketException

    app = FastrAPI()

    def guard(token: str = ""):
        if token != "ok":
            raise WebSocketException(code=1008)
        return token

    @app.websocket("/guarded")
    async def guarded(websocket: WebSocket, token: str = Depends(guard)):
        await websocket.accept()
        await websocket.close()

    @app.websocket("/raises")
    async def raises(websocket: WebSocket):
        await websocket.accept()
        await websocket.send_text("hi")
        raise WebSocketException(code=4000, reason="bye")

    @app.websocket("/crash")
    async def crash(websocket: WebSocket):
        await websocket.accept()
        raise RuntimeError("boom")

    @app.websocket("/returns")
    async def returns(websocket: WebSocket):
        await websocket.accept()
        await websocket.send_text("done")

    port = _app_with_port(app)
    assert _connect(port, "/guarded")[0] == 403
    assert _connect(port, "/guarded?token=ok")[0] == 101
    status, _, _, sock = _connect(port, "/raises")
    assert status == 101
    assert _recv_frame(sock) == (0x1, b"hi")
    opcode, payload = _recv_frame(sock)
    assert opcode == 0x8
    assert _close_code(payload) == (4000, "bye")
    status, _, _, sock = _connect(port, "/crash")
    opcode, payload = _recv_frame(sock)
    assert (opcode, _close_code(payload)[0]) == (0x8, 1011)
    status, _, _, sock = _connect(port, "/returns")
    assert _recv_frame(sock) == (0x1, b"done")
    opcode, payload = _recv_frame(sock)
    assert (opcode, _close_code(payload)[0]) == (0x8, 1000)


def test_websocket_exception_handler_and_validation_error():
    from fastrapi import WebSocket
    from fastrapi.exceptions import WebSocketRequestValidationError

    app = FastrAPI()
    seen = []

    class Boom(Exception):
        pass

    @app.exception_handler(Boom)
    async def on_boom(websocket, exc):
        seen.append(type(exc).__name__)
        await websocket.close(code=4001)

    @app.exception_handler(WebSocketRequestValidationError)
    async def on_invalid(websocket, exc):
        seen.append(exc.errors()[0]["loc"])
        await websocket.close(code=1008)

    @app.websocket("/boom")
    async def boom(websocket: WebSocket):
        await websocket.accept()
        raise Boom()

    @app.websocket("/typed")
    async def typed(websocket: WebSocket, count: int):
        await websocket.accept()

    port = _app_with_port(app)
    status, _, _, sock = _connect(port, "/boom")
    opcode, payload = _recv_frame(sock)
    assert (opcode, _close_code(payload)[0]) == (0x8, 4001)
    assert _connect(port, "/typed?count=x")[0] == 403
    assert seen == ["Boom", ("query", "count")]


def test_websocket_route_name_and_dependencies():
    from fastrapi import APIRouter, Depends, WebSocket

    app = FastrAPI()
    router = APIRouter()
    calls = []

    def audit(websocket: WebSocket):
        calls.append(websocket.url.path)

    @router.websocket("/chat/{room}", name="chat", dependencies=[Depends(audit)])
    async def chat(websocket: WebSocket, room: str):
        await websocket.accept()
        await websocket.send_text(str(websocket.url_for("chat", room="b")))
        await websocket.close()

    app.include_router(router)
    port = _app_with_port(app)
    assert app.url_path_for("chat", room="a") == "/chat/a"
    status, _, _, sock = _connect(port, "/chat/a")
    assert status == 101
    assert _recv_frame(sock) == (0x1, f"ws://127.0.0.1:{port}/chat/b".encode())
    assert calls == ["/chat/a"]


def test_websocket_raw_asgi_messages_and_states():
    from starlette.websockets import WebSocketState

    from fastrapi import WebSocket

    app = FastrAPI()

    @app.websocket("/raw")
    async def raw(websocket: WebSocket):
        states = [websocket.client_state.name, websocket.application_state.name]
        connect = await websocket.receive()
        await websocket.send({"type": "websocket.accept"})
        states.append(websocket.application_state.name)
        message = await websocket.receive()
        await websocket.send({"type": "websocket.send", "text": f"{connect['type']}|{message['text']}|{','.join(states)}"})
        await websocket.send({"type": "websocket.send", "bytes": b"raw"})
        disconnect = await websocket.receive()
        assert disconnect["type"] == "websocket.disconnect"
        assert websocket.client_state == WebSocketState.DISCONNECTED

    port = _app_with_port(app)
    status, _, _, sock = _connect(port, "/raw")
    assert status == 101
    _send(sock, "ping")
    assert _recv_frame(sock) == (0x1, b"websocket.connect|ping|CONNECTING,CONNECTING,CONNECTED")
    assert _recv_frame(sock) == (0x2, b"raw")
    _send(sock, struct.pack("!H", 1000), opcode=0x8)


def test_websocket_denial_response():
    from fastrapi import WebSocket
    from fastrapi.responses import JSONResponse

    app = FastrAPI()

    @app.websocket("/deny")
    async def deny(websocket: WebSocket):
        await websocket.send_denial_response(JSONResponse({"detail": "nope"}, status_code=401))

    @app.websocket("/deny-raw")
    async def deny_raw(websocket: WebSocket):
        await websocket.send({"type": "websocket.http.response.start", "status": 418, "headers": [(b"x-why", b"tea")]})
        await websocket.send({"type": "websocket.http.response.body", "body": b"short"})

    port = _app_with_port(app)
    status, head, rest, sock = _connect(port, "/deny")
    assert status == 401
    for _ in range(5):
        if b"nope" in rest:
            break
        rest += sock.recv(4096)
    assert b"nope" in rest
    status, head, _, _ = _connect(port, "/deny-raw")
    assert status == 418
    assert "x-why: tea" in head.lower()
