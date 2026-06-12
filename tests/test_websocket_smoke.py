import base64
import os
import socket
import struct
import threading
import time

from fastrapi import FastrAPI


def _free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def _serve(app: FastrAPI, port: int) -> None:
    thread = threading.Thread(
        target=lambda: app.serve(host="127.0.0.1", port=port),
        daemon=True,
    )
    thread.start()
    deadline = time.time() + 10.0
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5) as sock:
                sock.sendall(
                    (
                        "GET /api-docs/openapi.json HTTP/1.1\r\n"
                        f"Host: 127.0.0.1:{port}\r\n"
                        "Connection: close\r\n"
                        "\r\n"
                    ).encode()
                )
                if b" 200 " in sock.recv(512):
                    return
        except Exception:
            time.sleep(0.05)
    raise RuntimeError(f"server on port {port} never became ready")


def _send_text(sock: socket.socket, text: str) -> None:
    payload = text.encode()
    mask = os.urandom(4)
    header = bytearray([0x81])
    if len(payload) < 126:
        header.append(0x80 | len(payload))
    elif len(payload) < 65536:
        header.append(0x80 | 126)
        header.extend(struct.pack("!H", len(payload)))
    else:
        header.append(0x80 | 127)
        header.extend(struct.pack("!Q", len(payload)))
    masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    sock.sendall(bytes(header) + mask + masked)


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


def test_websocket_echo_smoke():
    app = FastrAPI()

    @app.websocket("/ws")
    async def ws_endpoint(ws):
        await ws.accept()
        message = await ws.receive_text()
        await ws.send_text(f"echo:{message}")
        await ws.close()

    port = _free_port()
    _serve(app, port)

    key = base64.b64encode(os.urandom(16)).decode()
    with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
        sock.sendall(
            (
                "GET /ws HTTP/1.1\r\n"
                f"Host: 127.0.0.1:{port}\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n"
                "\r\n"
            ).encode()
        )
        response = sock.recv(4096)
        assert b" 101 " in response

        _send_text(sock, "ping")
        opcode, payload = _recv_frame(sock)
        assert opcode == 0x1
        assert payload.decode() == "echo:ping"
