import base64
import json
import os
import socket
import struct

from test_websocket_smoke import _free_port, _recv_frame, _serve

from fastrapi import FastrAPI


def _connect(port, path, headers=()):
    key = base64.b64encode(os.urandom(16)).decode()
    sock = socket.create_connection(("127.0.0.1", port), timeout=5)
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
