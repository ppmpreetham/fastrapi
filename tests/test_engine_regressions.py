import socket
import time
import uuid

import pytest

from fastrapi import Depends, FastrAPI


def test_bind_failure_raises_oserror():
    app = FastrAPI()

    @app.get("/")
    def root():
        return {}

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as taken:
        taken.bind(("127.0.0.1", 0))
        taken.listen()
        port = taken.getsockname()[1]
        with pytest.raises(OSError, match="failed to bind"):
            app.serve("127.0.0.1", port)


def test_rate_limit_zero_is_rejected_at_registration(app):
    with pytest.raises(ValueError, match="rate_limit must be greater than 0"):

        @app.get("/limited", rate_limit=0)
        def limited():
            return {}


def test_override_is_live_for_async_handler_with_sync_dependency(client, app):
    def original():
        return "original"

    def replacement():
        return "replacement"

    @app.get("/async-override")
    async def endpoint(value=Depends(original)):
        return {"value": value}

    assert client.get("/async-override").json() == {"value": "original"}
    app.dependency_overrides[original] = replacement
    assert client.get("/async-override").json() == {"value": "replacement"}
    app.dependency_overrides.clear()
    assert client.get("/async-override").json() == {"value": "original"}


def test_uuid_path_convertor_matches_like_starlette(client, app):
    @app.get("/items/{item_id:uuid}")
    def item(item_id: str):
        return {"item_id": item_id}

    dashed = str(uuid.uuid4())
    assert client.get(f"/items/{dashed}").status_code == 200
    assert client.get(f"/items/{uuid.uuid4().hex}").status_code == 200
    assert client.get("/items/not-a-uuid").status_code == 404
    assert client.get(f"/items/{dashed}-0000").status_code == 404


def test_datastructures_submodule_exports():
    from fastrapi.datastructures import URL, FormData, UploadFile

    assert {URL.__name__, FormData.__name__, UploadFile.__name__} == {
        "URL",
        "FormData",
        "UploadFile",
    }


def test_websocket_frame_split_across_server_sends_is_received_intact():
    import asyncio
    import base64
    import os
    import struct

    from test_websocket_smoke import _free_port, _recv_frame, _serve

    app = FastrAPI()

    @app.websocket("/ws")
    async def ws_endpoint(ws):
        await ws.accept()

        async def ticker():
            for _ in range(40):
                await ws.send_text("tick")
                await asyncio.sleep(0.01)

        ticks = asyncio.ensure_future(ticker())
        message = await ws.receive_text()
        await ws.send_text(f"got:{len(message)}")
        await ticks

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
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        assert b" 101 " in sock.recv(4096)

        payload = b"x" * 300
        mask = os.urandom(4)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        frame = bytes([0x81, 0x80 | 126]) + struct.pack("!H", len(payload)) + mask + masked
        sock.sendall(frame[:2])
        time.sleep(0.15)
        sock.sendall(frame[2:])

        replies = []
        while "got:300" not in replies:
            opcode, data = _recv_frame(sock)
            assert opcode == 0x1, f"unexpected opcode {opcode}"
            replies.append(data.decode())


def test_rate_limit_rejects_requests_over_the_limit(client, app):
    @app.get("/limited", rate_limit=2)
    def limited():
        return {"ok": True}

    statuses = [client.get("/limited").status_code for _ in range(5)]
    assert statuses[0] == 200
    assert 429 in statuses
    assert set(statuses) <= {200, 429}


def test_async_status_and_validation_handlers_are_awaited(client, app):
    from fastrapi.exceptions import RequestValidationError

    @app.exception_handler(404)
    async def not_found_handler(request, exc):
        return {"missing": True}

    @app.exception_handler(RequestValidationError)
    async def validation_handler(request, exc):
        return {"invalid": len(exc.errors())}

    @app.get("/needs-int")
    def needs_int(value: int):
        return {"value": value}

    assert client.get("/no-such-route").json() == {"missing": True}
    assert client.get("/needs-int", params={"value": "x"}).json() == {"invalid": 1}
