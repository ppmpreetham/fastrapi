from contextlib import asynccontextmanager

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import BaseModel

import fastrapi


class Item(BaseModel):
    name: str


def build(lib, events):
    @asynccontextmanager
    async def lifespan(app):
        events.append("start")
        yield {"resource": "ready"}
        events.append("stop")

    app = (lib.FastAPI if lib is fastapi else lib.FastrAPI)(lifespan=lifespan)
    sse = __import__(f"{lib.__name__}.sse", fromlist=["_"])

    @app.middleware("http")
    async def tag(request, call_next):
        response = await call_next(request)
        response.headers["x-tag"] = "set"
        return response

    @app.get("/")
    def root():
        return {"hello": "world"}

    @app.post("/items")
    async def create(item: Item):
        return item

    @app.get("/state")
    def state(request: lib.Request):
        return {"resource": request.state.resource}

    @app.get("/echo")
    def echo(q: str, x_token: str = lib.Header(), session: str = lib.Cookie()):
        return {"q": q, "token": x_token, "session": session}

    @app.get("/events", response_class=sse.EventSourceResponse)
    async def events_stream():
        for index in range(3):
            yield {"index": index}

    return app


def observe(client):
    responses = [
        client.get("/"),
        client.post("/items", json={"name": "x"}),
        client.post("/items", json={}),
        client.get("/missing"),
        client.get("/echo", params={"q": "ü"}, headers={"x-token": "t"}, cookies={"session": "s"}),
        client.get("/events"),
    ]
    return [(r.status_code, r.headers.get("content-type"), r.headers.get("x-tag"), r.content) for r in responses]


def test_plain_test_client_matches_fastapi():
    assert observe(TestClient(build(fastrapi, []))) == observe(TestClient(build(fastapi, [])))


def test_lifespan_matches_fastapi():
    def run(lib):
        events = []
        with TestClient(build(lib, events)) as client:
            state = client.get("/state").json()
            events.append("request")
        return events, state

    assert run(fastrapi) == run(fastapi)


def test_startup_failure_propagates():
    for lib in (fastapi, fastrapi):

        @asynccontextmanager
        async def broken(app):
            raise RuntimeError("boom")
            yield

        app = (lib.FastAPI if lib is fastapi else lib.FastrAPI)(lifespan=broken)
        with pytest.raises(RuntimeError, match="boom"), TestClient(app):
            pass


def test_asgi_middleware_matches_fastapi():
    def build_with_asgi(lib):
        app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()

        class Header:
            def __init__(self, app):
                self.app = app

            async def __call__(self, scope, receive, send):
                async def wrapped(message):
                    if message["type"] == "http.response.start":
                        message["headers"] = [*message["headers"], (b"x-asgi", b"yes")]
                    await send(message)

                await self.app(scope, receive, wrapped)

        app.add_middleware(Header)

        @app.get("/")
        def root():
            return {"ok": True}

        return app

    def run(lib):
        response = TestClient(build_with_asgi(lib)).get("/")
        return response.status_code, response.headers.get("x-asgi"), response.json()

    assert run(fastrapi) == run(fastapi)


def test_mounted_routing_matches_fastapi():
    def run(lib):
        child = lib.FastAPI() if lib is fastapi else lib.FastrAPI()

        @child.get("/hello/{name}")
        def hello(name: str):
            return {"name": name}

        parent = fastapi.FastAPI()
        parent.mount("/sub", child)
        client = TestClient(parent)
        return [(r.status_code, r.content) for r in (client.get("/sub/hello/x"), client.get("/sub/missing"))]

    assert run(fastrapi) == run(fastapi)
