import asyncio
import functools
import json
import subprocess
import sys

import fastapi
import pytest
from conftest import LiveServerTestClient
from fastapi.testclient import TestClient
from starlette.exceptions import HTTPException as StarletteHTTPException
from starlette.responses import PlainTextResponse
from starlette.websockets import WebSocketDisconnect

import fastrapi


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def both(run):
    assert run(fastrapi) == run(fastapi)


def http_client(lib, app):
    """FastAPI in process, fastrapi on its native server."""
    return TestClient(app) if lib is fastapi else LiveServerTestClient(app)


def summary(response):
    return response.status_code, response.headers.get("content-type"), response.text


def ws_app(lib):
    app = new_app(lib)

    @app.websocket("/binary-json")
    async def binary_json(websocket: lib.WebSocket):
        await websocket.accept()
        try:
            await websocket.receive_json()
        except Exception as exc:
            await websocket.send_text(repr(exc))

    @app.websocket("/iter-text")
    async def iter_text(websocket: lib.WebSocket):
        await websocket.accept()
        seen = []
        try:
            async for message in websocket.iter_text():
                seen.append(message)
        except Exception as exc:
            seen.append(repr(exc))
        await websocket.send_text(json.dumps(seen))

    @app.websocket("/non-finite")
    async def non_finite(websocket: lib.WebSocket):
        await websocket.accept()
        await websocket.send_json({"a": float("nan"), "b": float("inf"), "c": -float("inf")})

    @app.websocket("/close-first")
    async def close_first(websocket: lib.WebSocket):
        await websocket.close(4003, "no")

    @app.websocket("/before-accept")
    async def before_accept(websocket: lib.WebSocket):
        errors = []

        async def drain_iter():
            async for _ in websocket.iter_text():
                pass

        for call in (websocket.receive_text, websocket.receive_bytes, websocket.receive_json, drain_iter, lambda: websocket.send_text("x")):
            try:
                await call()
            except Exception as exc:
                errors.append([isinstance(exc, RuntimeError), str(exc)])
        await websocket.accept()
        await websocket.send_text(json.dumps(errors))

    return app


def ws_result(lib, path, send=()):
    with TestClient(ws_app(lib)).websocket_connect(path) as websocket:
        for kind, data in send:
            getattr(websocket, f"send_{kind}")(data)
        return websocket.receive_text()


def test_websocket_receive_on_the_wrong_frame_type_is_a_key_error():
    both(lambda lib: ws_result(lib, "/binary-json", [("bytes", b"{}")]))
    assert ws_result(fastrapi, "/binary-json", [("bytes", b"{}")]) == "KeyError('text')"
    both(lambda lib: ws_result(lib, "/iter-text", [("text", "a"), ("bytes", b"b")]))


def test_websocket_send_json_writes_non_finite_numbers():
    both(lambda lib: ws_result(lib, "/non-finite"))
    assert ws_result(fastrapi, "/non-finite") == '{"a":NaN,"b":Infinity,"c":-Infinity}'


def test_websocket_close_before_accept_keeps_the_reason():
    def run(lib):
        with pytest.raises(WebSocketDisconnect) as info:
            with TestClient(ws_app(lib)).websocket_connect("/close-first"):
                pass
        return info.value.code, info.value.reason

    both(run)


def test_websocket_receive_and_send_before_accept_raise():
    both(lambda lib: ws_result(lib, "/before-accept"))


def test_sync_middleware_returning_call_next_sends_the_body():
    def run(lib):
        app = new_app(lib)

        @app.middleware("http")
        def passthrough(request, call_next):
            return call_next(request)

        @app.get("/")
        def root():
            return {"ok": 1}

        response = TestClient(app).get("/")
        return response.status_code, response.text

    both(run)


def asgi_get(app, headers):
    scope = {
        "type": "http",
        "asgi": {"version": "3.0"},
        "http_version": "1.1",
        "method": "GET",
        "scheme": "http",
        "path": "/",
        "raw_path": b"/",
        "query_string": b"",
        "root_path": "",
        "headers": headers,
        "client": ("c", 1),
        "server": None,
    }
    sent = []

    async def receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(message):
        sent.append(message)

    asyncio.run(app(scope, receive, send))
    start = sent[0]
    return start["status"], dict(start["headers"]).get(b"content-type"), b"".join(m.get("body", b"") for m in sent[1:])


def test_https_redirect_without_a_host_is_starlettes_text_400():
    def run(lib):
        redirect = __import__(f"{lib.__name__}.middleware.httpsredirect", fromlist=["_"]).HTTPSRedirectMiddleware
        app = new_app(lib)
        app.add_middleware(redirect)
        return asgi_get(app, [(b"host", b"")])

    both(run)


@pytest.mark.parametrize("path", ["/docs", "/redoc", "/openapi.json"])
def test_docs_routes_answer_a_wrong_method_with_a_json_405(path):
    def run(lib):
        response = http_client(lib, new_app(lib)).post(path)
        allow = set(response.headers["allow"].replace(" ", "").split(","))
        return summary(response), response.headers["content-length"], allow

    both(run)


def test_404_and_405_use_the_http_exception_handler():
    def run(lib):
        app = new_app(lib)

        @app.exception_handler(StarletteHTTPException)
        async def handler(request, exc):
            return PlainTextResponse(f"custom {exc.status_code}", status_code=exc.status_code)

        @app.get("/only-get")
        def only_get():
            return {}

        client = http_client(lib, app)
        return summary(client.get("/nope")), summary(client.post("/only-get"))

    both(run)


def test_custom_404_handler_keeps_frontend_mounts(tmp_path):
    (tmp_path / "index.html").write_text("front")
    (tmp_path / "a.txt").write_text("A")

    def run(lib):
        app = new_app(lib)

        @app.exception_handler(404)
        async def not_found(request, exc):
            return PlainTextResponse("custom404", status_code=404)

        app.frontend("/", directory=tmp_path)
        client = http_client(lib, app)
        json_only = {"accept": "application/json"}
        return [
            (response.status_code, response.text)
            for response in (client.get("/a.txt"), client.get("/missing", headers=json_only), client.post("/missing"))
        ]

    both(run)
    assert run(fastrapi)[0] == (200, "A")


def test_frontend_accepts_any_path_like(tmp_path):
    (tmp_path / "index.html").write_text("front")

    class Directory:
        def __fspath__(self):
            return str(tmp_path)

    def run(lib):
        results = []
        for directory in (tmp_path, Directory()):
            app = new_app(lib)
            app.frontend("/", directory=directory)
            results.append(http_client(lib, app).get("/").text)
        return results

    both(run)


def test_route_paths_shared_by_methods_run_every_method():
    def run(lib):
        app = new_app(lib)

        @app.get("/x")
        def read():
            return {"get": 1}

        @app.post("/x")
        def create(item: dict):
            return {"post": item}

        client = http_client(lib, app)
        responses = [client.get("/x"), client.post("/x", json={"a": 1})]
        return [(response.status_code, response.text, "allow" in response.headers) for response in responses]

    both(run)


async def record(events, tag):
    events.append(tag)


class AsyncCallable:
    def __init__(self, events):
        self.events = events

    async def __call__(self):
        self.events.append("object")


def test_async_callable_and_partial_lifecycle_handlers_are_awaited():
    events = []
    app = fastrapi.FastrAPI(
        on_startup=[functools.partial(record, events, "partial"), AsyncCallable(events)],
        on_shutdown=[functools.partial(record, events, "shutdown")],
    )
    with LiveServerTestClient(app) as client:
        client.get("/openapi.json")
    assert events == ["partial", "object"]


def test_import_emits_no_deprecation_warnings():
    subprocess.run([sys.executable, "-W", "error", "-c", "import fastrapi"], check=True)
    wsgi = "from fastrapi.middleware.wsgi import WSGIMiddleware; assert WSGIMiddleware"
    subprocess.run([sys.executable, "-c", wsgi], check=True)
