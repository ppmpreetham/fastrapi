import asyncio
from collections import OrderedDict

import fastapi
import pytest
from fastapi.testclient import TestClient

import fastrapi
from conftest import LiveServerTestClient


class Boom(Exception):
    pass


def build(lib):
    app = lib.FastAPI()
    exceptions = lib.exceptions

    @app.exception_handler(exceptions.RequestValidationError)
    async def validation(request, exc):
        return lib.responses.JSONResponse(
            {"path": request.url.path, "body": exc.body, "count": len(exc.errors())},
            status_code=422,
        )

    @app.exception_handler(404)
    async def not_found(request, exc):
        return lib.responses.JSONResponse({"missing": request.url.path}, status_code=404)

    @app.exception_handler(Boom)
    def boom(request, exc):
        return lib.responses.JSONResponse({"boom": request.url.path}, status_code=418)

    @app.post("/validate")
    def validate(value: int = lib.Body(embed=True)):
        return {"value": value}

    @app.get("/boom")
    def raise_boom():
        raise Boom()

    def dependency():
        raise Boom()

    @app.get("/dependency", dependencies=[lib.Depends(dependency)])
    def with_dependency():
        return {}

    @app.get("/headers")
    def headers():
        raise lib.HTTPException(401, "nope", headers=OrderedDict([("x-a", "1")]))

    @app.get("/no-body")
    def no_body():
        raise lib.HTTPException(304)

    @app.get("/teapot")
    def teapot():
        raise lib.HTTPException(418)

    return app


def build_server_errors(lib, handled):
    app = lib.FastAPI()
    if handled:

        @app.exception_handler(500)
        async def server_error(request, exc):
            return lib.responses.JSONResponse({"error": type(exc).__name__}, status_code=500)

    @app.exception_handler(Exception)
    async def generic(request, exc):
        return lib.responses.JSONResponse({"generic": True}, status_code=500)

    @app.get("/crash")
    def crash():
        raise ValueError("crash")

    @app.get("/http")
    def http():
        raise lib.HTTPException(400, "bad")

    return app


@pytest.fixture(scope="module")
def clients():
    expected = TestClient(build(fastapi), raise_server_exceptions=False)
    with LiveServerTestClient(build(fastrapi)) as actual:
        yield expected, actual


CASES = [
    ("post", "/validate", {"json": {"value": "x"}}),
    ("get", "/unknown", {}),
    ("get", "/boom", {}),
    ("get", "/dependency", {}),
    ("get", "/headers", {}),
    ("get", "/no-body", {}),
    ("get", "/teapot", {}),
]


@pytest.mark.parametrize("case", CASES, ids=[url for _, url, _ in CASES])
def test_matches_fastapi(clients, case):
    expected_client, actual_client = clients
    method, url, kwargs = case
    expected = getattr(expected_client, method)(url, **kwargs)
    actual = getattr(actual_client, method)(url, **kwargs)
    assert actual.status_code == expected.status_code
    assert actual.content == expected.content
    assert actual.headers.get("x-a") == expected.headers.get("x-a")


def test_server_error_handlers_match_fastapi():
    expected = TestClient(build_server_errors(fastapi, handled=True), raise_server_exceptions=False)
    with LiveServerTestClient(build_server_errors(fastrapi, handled=True)) as actual:
        for url in ("/crash", "/http"):
            assert actual.get(url).json() == expected.get(url).json()


def test_default_server_error_body():
    with LiveServerTestClient(build(fastrapi)) as client:
        app = client.app

        @app.get("/fail")
        def fail():
            raise ValueError("fail")

        response = client.get("/fail")
    assert response.status_code == 500
    assert response.text == "Internal Server Error"
    assert response.headers["content-type"].startswith("text/plain")


def test_exception_classes_match_fastapi():
    from fastrapi.exceptions import FastAPIError, RequestValidationError, ValidationException

    class Custom(FastAPIError):
        pass

    assert str(Custom("msg")) == "msg"
    assert isinstance(fastrapi.HTTPException(400), __import__("starlette.exceptions").exceptions.HTTPException)
    exc = RequestValidationError([{"loc": ("query", "q")}], body={"a": 1})
    assert exc.body == {"a": 1}
    assert str(ValidationException([{"x": 1}])) == str(fastapi.exceptions.ValidationException([{"x": 1}]))
    assert fastrapi.HTTPException(401, headers=OrderedDict(a="b")).headers == {"a": "b"}


def test_default_handlers_are_async():
    from fastrapi.exception_handlers import http_exception_handler, request_validation_exception_handler
    from fastrapi.exceptions import RequestValidationError

    no_content = asyncio.run(http_exception_handler(None, fastrapi.HTTPException(204)))
    assert no_content.status_code == 204 and no_content.body == b""
    invalid = asyncio.run(request_validation_exception_handler(None, RequestValidationError([{"loc": ("q",)}])))
    assert invalid.status_code == 422 and invalid.body == b'{"detail":[{"loc":["q"]}]}'
