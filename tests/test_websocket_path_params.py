# Websocket path params. The existing smoke test only covers a static `/ws` route, so
# nothing exercised param *ordering*, which is what `match_path_params` is doing.
#
# Injection goes through a `Depends()` sub-dependency on purpose: a websocket handler's
# own plain arguments are not resolved against path/query (unlike FastAPI) - only
# `Depends`/`Security` markers are.

import base64
import json
import os
import socket
import struct
import threading
import time

from fastrapi import Depends, FastrAPI


def _free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def _serve(app: FastrAPI, port: int) -> None:
    threading.Thread(
        target=lambda: app.serve(host="127.0.0.1", port=port), daemon=True
    ).start()
    deadline = time.time() + 10.0
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5) as sock:
                sock.sendall(
                    (
                        "GET /openapi.json HTTP/1.1\r\n"
                        f"Host: 127.0.0.1:{port}\r\n"
                        "Connection: close\r\n"
                        "\r\n"
                    ).encode()
                )
                if b" 200 " in sock.recv(512):
                    return
        except Exception:
            time.sleep(0.05)
    raise RuntimeError("server never became ready")


def _recv_frame(sock: socket.socket) -> tuple[int, bytes]:
    first = sock.recv(2)
    assert len(first) == 2
    opcode = first[0] & 0x0F
    length = first[1] & 0x7F
    if length == 126:
        length = struct.unpack("!H", sock.recv(2))[0]
    elif length == 127:
        length = struct.unpack("!Q", sock.recv(8))[0]
    payload = bytearray()
    while len(payload) < length:
        payload.extend(sock.recv(length - len(payload)))
    return opcode, bytes(payload)


def _handshake(port: int, path: str) -> socket.socket:
    key = base64.b64encode(os.urandom(16)).decode()
    sock = socket.create_connection(("127.0.0.1", port), timeout=5)
    sock.sendall(
        (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: 127.0.0.1:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "\r\n"
        ).encode()
    )
    assert b" 101 " in sock.recv(4096)
    return sock


def test_ws_two_path_params():
    app = FastrAPI()

    def pair(room: str = "", user: str = ""):
        return f"{room}|{user}"

    @app.websocket("/ws/{room}/{user}")
    async def ws_endpoint(websocket, dep=Depends(pair)):
        await websocket.accept()
        await websocket.send_text(dep)
        await websocket.close()

    port = _free_port()
    _serve(app, port)

    sock = _handshake(port, "/ws/lobby/alice")
    opcode, payload = _recv_frame(sock)
    assert opcode == 0x1
    assert payload.decode() == "lobby|alice"
    sock.close()


def test_ws_single_path_param():
    app = FastrAPI()

    def one(room_id: str = ""):
        return f"id={room_id}"

    @app.websocket("/room/{room_id}")
    async def ws_endpoint(websocket, dep=Depends(one)):
        await websocket.accept()
        await websocket.send_text(dep)
        await websocket.close()

    port = _free_port()
    _serve(app, port)

    sock = _handshake(port, "/room/42")
    opcode, payload = _recv_frame(sock)
    assert opcode == 0x1
    assert payload.decode() == "id=42"
    sock.close()


def test_ws_param_plus_query():
    app = FastrAPI()

    def both(room: str = "", token: str = ""):
        return json.dumps({"room": room, "token": token})

    @app.websocket("/ws2/{room}")
    async def ws_endpoint(websocket, dep=Depends(both)):
        await websocket.accept()
        await websocket.send_text(dep)
        await websocket.close()

    port = _free_port()
    _serve(app, port)

    sock = _handshake(port, "/ws2/lobby?token=tok")
    opcode, payload = _recv_frame(sock)
    assert opcode == 0x1
    assert json.loads(payload) == {"room": "lobby", "token": "tok"}
    sock.close()
