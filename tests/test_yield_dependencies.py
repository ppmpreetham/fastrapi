import asyncio
import socket
import threading
import time

import httpx

from fastrapi import Depends, FastrAPI


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def _wait_for_port(port: int, timeout: float = 10.0) -> None:
    """Block until something is listening, without issuing an HTTP request.

    A readiness ping would re-run the dependency under test and pollute the
    event log these tests assert on, so only probe the socket.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket() as s:
            s.settimeout(0.5)
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.05)
    raise RuntimeError(f"server on port {port} never became ready")


def _get(app: FastrAPI, path: str) -> httpx.Response:
    """Serve `app` on an ephemeral port in a daemon thread and GET `path`."""
    port = _free_port()
    threading.Thread(
        target=lambda: app.serve(host="127.0.0.1", port=port),
        daemon=True,
    ).start()
    _wait_for_port(port)
    return httpx.get(f"http://127.0.0.1:{port}{path}", timeout=5.0)


def test_sync_yield_dependency():
    app = FastrAPI()
    events = []

    def get_sync_db():
        events.append("sync db open")
        yield "sync_db_session"
        events.append("sync db closed")

    @app.get("/sync-yield")
    def sync_yield_route(db_session: str = Depends(get_sync_db)):
        events.append(f"sync route executing with {db_session}")
        return {"msg": db_session}

    response = _get(app, "/sync-yield")
    assert response.status_code == 200
    assert response.json() == {"msg": "sync_db_session"}
    assert events == [
        "sync db open",
        "sync route executing with sync_db_session",
        "sync db closed",
    ]


def test_async_yield_dependency():
    app = FastrAPI()
    events = []

    async def get_async_db():
        events.append("async db open")
        await asyncio.sleep(0.01)
        yield "async_db_session"
        await asyncio.sleep(0.01)
        events.append("async db closed")

    @app.get("/async-yield")
    async def async_yield_route(db_session: str = Depends(get_async_db)):
        events.append(f"async route executing with {db_session}")
        return {"msg": db_session}

    response = _get(app, "/async-yield")
    assert response.status_code == 200
    assert response.json() == {"msg": "async_db_session"}
    assert events == [
        "async db open",
        "async route executing with async_db_session",
        "async db closed",
    ]


def test_mixed_yield_dependencies():
    app = FastrAPI()
    events = []

    def sync_dep():
        events.append("sync open")
        yield "sync_val"
        events.append("sync closed")

    async def async_dep():
        events.append("async open")
        yield "async_val"
        events.append("async closed")

    @app.get("/mixed")
    async def mixed_route(s: str = Depends(sync_dep), a: str = Depends(async_dep)):
        events.append(f"route {s} {a}")
        return {"s": s, "a": a}

    response = _get(app, "/mixed")
    assert response.status_code == 200
    assert response.json() == {"s": "sync_val", "a": "async_val"}

    # FastrAPI executes dependencies left to right, but teardowns should be reversed (LIFO)
    assert events == [
        "sync open",
        "async open",
        "route sync_val async_val",
        "async closed",
        "sync closed",
    ]
