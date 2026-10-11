"""Tests for FastAPI-parity features added in this cycle."""
import json

from typing import List

from fastrapi import FastrAPI, Depends, Request
from pydantic import BaseModel
from fastrapi.responses import FileResponse, JSONResponse
from fastrapi.responses import EventSourceResponse
from fastrapi.sse import ServerSentEvent


def test_file_response(client, app, tmp_path):
    f = tmp_path / "hello.txt"
    f.write_text("file content here")

    @app.get("/dl")
    def dl() -> FileResponse:
        return FileResponse(str(f), filename="hello.txt")

    r = client.get("/dl")
    assert r.status_code == 200
    assert r.text == "file content here"


def test_file_response_cache_and_ranges(client, app, tmp_path):
    f = tmp_path / "range.txt"
    f.write_text("0123456789")

    @app.get("/range")
    def range_route() -> FileResponse:
        return FileResponse(str(f))

    full = client.get("/range")
    assert full.status_code == 200
    assert full.headers["accept-ranges"] == "bytes"
    assert full.headers["etag"]
    assert full.headers["last-modified"]

    partial = client.get("/range", headers={"Range": "bytes=2-5"})
    assert partial.status_code == 206
    assert partial.text == "2345"
    assert partial.headers["content-range"] == "bytes 2-5/10"

    unchanged = client.get("/range", headers={"If-None-Match": full.headers["etag"]})
    assert unchanged.status_code == 304


def test_file_response_multi_range(client, app, tmp_path):
    f = tmp_path / "multi.txt"
    f.write_text("0123456789")

    @app.get("/multi")
    def multi_route() -> FileResponse:
        return FileResponse(str(f))

    response = client.get("/multi", headers={"Range": "bytes=0-1,8-9"})
    assert response.status_code == 206
    assert "multipart/byteranges" in response.headers["content-type"]
    assert "01" in response.text
    assert "89" in response.text


def test_sse_response_formatting(client, app):
    @app.get("/events")
    def events() -> EventSourceResponse:
        return EventSourceResponse(
            iter([ServerSentEvent(data="hello", event="message", id="1", retry=25)]),
            ping=None,
            sep="\n",
        )

    response = client.get("/events")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/event-stream")
    assert response.text == 'event: message\ndata: "hello"\nid: 1\nretry: 25\n\n'




def test_repeated_query_list_params(client, app):
    @app.get("/qtags")
    def qtags(tags: List[str] = []):
        return {"tags": tags}

    r = client.get("/qtags?tags=a&tags=b&tags=c")
    assert r.status_code == 200
    assert r.json() == {"tags": ["a", "b", "c"]}


def test_repeated_query_int_list_coercion(client, app):
    @app.get("/qnums")
    def qnums(q: List[int] = []):
        return {"q": q}

    r = client.get("/qnums?q=1&q=2&q=3")
    assert r.status_code == 200
    assert r.json() == {"q": [1, 2, 3]}


def test_dependency_overrides(client, app):
    def real_dep():
        return "real"

    app.dependency_overrides[real_dep] = lambda: "OVERRIDE"

    @app.get("/d")
    def d_route(v=Depends(real_dep)):
        return {"v": v}

    r = client.get("/d")
    assert r.status_code == 200
    assert r.json()["v"] == "OVERRIDE"


def test_custom_exception_handler(client, app):
    class MyError(Exception):
        pass

    @app.get("/boom")
    def boom():
        raise MyError("kapow")

    @app.exception_handler(MyError)
    def my_handler(request, exc):
        return JSONResponse({"caught": str(exc)}, status_code=400)

    r = client.get("/boom")
    assert r.status_code == 400
    assert r.json() == {"caught": "kapow"}


def test_injected_request_body_and_json(client, app):
    @app.post("/inspect")
    async def inspect(request: Request):
        raw = await request.body()
        data = await request.json()
        return {"blen": len(raw), "echo": data}

    payload = json.dumps({"k": [1, 2]})
    r = client.post(
        "/inspect",
        content=payload,
        headers={"Content-Type": "application/json"},
    )
    assert r.status_code == 200
    body = r.json()
    assert body["blen"] == len(payload)
    assert body["echo"] == {"k": [1, 2]}


def test_structured_validation_error(client, app):
    @app.get("/valid")
    def valid(page: int = 1):
        return {"page": page}

    r = client.get("/valid?page=abc")
    assert r.status_code == 422
    detail = r.json()["detail"]
    assert isinstance(detail, list) and detail
    first = detail[0]
    assert first["loc"][:2] == ["query", "page"]
    assert first["type"] == "int_parsing"
    assert "msg" in first


def test_mount_sub_application(app):
    sub = FastrAPI()

    @sub.get("/inner")
    def inner():
        return {"from": "sub"}

    app.mount("/api", sub)

    from tests.conftest import LiveServerTestClient

    with LiveServerTestClient(app) as c:
        assert c.get("/api/inner").json() == {"from": "sub"}


def test_starlette_style_middleware(app):
    class Blocker:
        def __init__(self, path="/secret"):
            self.path = path

        async def __call__(self, request, call_next):
            if request.scope["path"] == self.path:
                return JSONResponse({"blocked": True})
            return await call_next(request)

    app.add_middleware(Blocker)

    @app.get("/secret")
    def secret():
        return {"top": "secret"}

    @app.get("/open")
    def open_route():
        return {"ok": True}

    from tests.conftest import LiveServerTestClient

    with LiveServerTestClient(app) as c:
        blocked = c.get("/secret")
        assert blocked.status_code == 200
        assert blocked.json() == {"blocked": True}

        passed = c.get("/open")
        assert passed.status_code == 200
        assert passed.json() == {"ok": True}

def test_response_model_none_passthrough(client, app):
    """`response_model=None` returns wrapper responses untouched and skips
    any serialization filtering (FastAPI raw passthrough parity)."""
    from fastrapi.responses import JSONResponse

    @app.get("/raw", response_model=None)
    def raw():
        return JSONResponse(
            {"anything": ["goes", 1, True]},
            status_code=201,
            headers={"x-custom": "yes"},
        )

    r = client.get("/raw")
    assert r.status_code == 201
    assert r.json() == {"anything": ["goes", 1, True]}
    assert r.headers["x-custom"] == "yes"
