import pytest
import asyncio
from fastrapi import FastrAPI, Request
from fastrapi.testclient import TestClient

def test_sync_yield_dependency():
    app = FastrAPI()
    events = []

    def get_sync_db():
        events.append("sync db open")
        yield "sync_db_session"
        events.append("sync db closed")

    @app.get("/sync-yield")
    def sync_yield_route(db_session: str = get_sync_db):
        events.append(f"sync route executing with {db_session}")
        return {"msg": db_session}

    client = TestClient(app)
    response = client.get("/sync-yield")
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
    async def async_yield_route(db_session: str = get_async_db):
        events.append(f"async route executing with {db_session}")
        return {"msg": db_session}

    client = TestClient(app)
    response = client.get("/async-yield")
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
    async def mixed_route(s: str = sync_dep, a: str = async_dep):
        events.append(f"route {s} {a}")
        return {"s": s, "a": a}

    client = TestClient(app)
    response = client.get("/mixed")
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
