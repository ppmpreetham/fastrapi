import fastapi
import pytest

import fastrapi


@pytest.mark.parametrize("code", [None, "default", "2XX", 100, 200, 204, 205, 304, "201", 404])
def test_body_allowed_for_status_code(code):
    from fastapi.utils import is_body_allowed_for_status_code as expected
    from fastrapi.utils import is_body_allowed_for_status_code as actual

    assert actual(code) == expected(code)


def test_status_module_matches_fastapi():
    import warnings

    from fastrapi import status

    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        assert status.HTTP_422_UNPROCESSABLE_ENTITY == fastapi.status.HTTP_422_UNPROCESSABLE_ENTITY
    assert status.HTTP_418_IM_A_TEAPOT == 418 and status.WS_1008_POLICY_VIOLATION == 1008


def test_redirect_response_accepts_url_objects(client, app):
    from fastrapi import Request
    from fastrapi.responses import RedirectResponse

    @app.get("/target")
    def target():
        return {}

    @app.get("/redirect")
    def redirect(request: Request):
        return RedirectResponse(request.url_for("target"))

    response = client.get("/redirect", follow_redirects=False)
    assert response.status_code == 307
    assert response.headers["location"].endswith("/target")


def test_response_headers_accept_mappings(client, app):
    from types import MappingProxyType

    from fastrapi.responses import FileResponse, JSONResponse, PlainTextResponse, RedirectResponse

    headers = MappingProxyType({"x-a": "1"})
    assert JSONResponse({}, headers=headers).headers["x-a"] == "1"
    assert RedirectResponse("/x", headers=headers).headers["x-a"] == "1"
    assert FileResponse(__file__, headers=headers).headers["x-a"] == "1"

    @app.get("/mapped")
    def mapped():
        return PlainTextResponse("ok", headers=headers)

    assert client.get("/mapped").headers["x-a"] == "1"


def test_mount_name_is_positional(tmp_path):
    from fastrapi.staticfiles import StaticFiles

    app = fastrapi.FastAPI()
    app.mount("/static", StaticFiles(directory=str(tmp_path)), "static")


def test_webhooks_default_router():
    assert "webhooks" not in fastrapi.FastAPI().openapi()
    app = fastrapi.FastAPI()

    @app.webhooks.post("new-subscription")
    def new_subscription(body: dict):
        return None

    assert "new-subscription" in app.openapi()["webhooks"]


def test_docs_defaults_match_fastapi(client, app):
    assert client.get("/scalar").status_code == 404
    assert client.get("/elements").status_code == 404
    assert client.get("/docs").status_code == 200 and client.get("/redoc").status_code == 200


def test_api_route_methods_are_sets():
    from fastapi.routing import APIRoute as Expected

    from fastrapi.routing import APIRoute

    assert APIRoute("/x", lambda: 1, methods=["get", "post"]).methods == Expected("/x", lambda: 1, methods=["get", "post"]).methods
    assert APIRoute("/x", lambda: 1).methods == {"GET"}
    app = fastrapi.FastAPI()

    @app.get("/y")
    def y():
        return 1

    assert next(route for route in app.routes if getattr(route, "path", "") == "/y").methods == {"GET"}


def build_uploads(lib):
    from typing import Annotated, Optional

    app = lib.FastAPI()

    @app.post("/bytes")
    def upload_bytes(data: Annotated[bytes, lib.File()], many: Annotated[Optional[list[bytes]], lib.File()] = None):
        return {"data": data.decode(), "many": [item.decode() for item in many or []]}

    return app


def test_bytes_file_params_match_fastapi():
    from fastapi.testclient import TestClient

    from conftest import LiveServerTestClient

    files = [("data", ("a.txt", b"hello")), ("many", ("b.txt", b"x")), ("many", ("c.txt", b"y"))]
    expected = TestClient(build_uploads(fastapi)).post("/bytes", files=files)
    with LiveServerTestClient(build_uploads(fastrapi)) as client:
        actual = client.post("/bytes", files=files)
    assert (actual.status_code, actual.json()) == (expected.status_code, expected.json())


def build_sub_response(lib):
    app = lib.FastAPI()

    @app.get("/sub")
    def sub(response: lib.responses.JSONResponse):
        response.headers["x-sub"] = "1"
        response.status_code = 201
        return {"type": type(response).__name__}

    return app


def test_response_subclass_param_matches_fastapi():
    from fastapi.testclient import TestClient

    from conftest import LiveServerTestClient

    expected = TestClient(build_sub_response(fastapi)).get("/sub")
    with LiveServerTestClient(build_sub_response(fastrapi)) as client:
        actual = client.get("/sub")
    assert actual.status_code == expected.status_code
    assert actual.headers["x-sub"] == expected.headers["x-sub"]


def test_default_exception_handlers_are_registered(client, app):
    from fastrapi.exception_handlers import http_exception_handler, request_validation_exception_handler
    from fastrapi.exceptions import RequestValidationError
    from starlette.exceptions import HTTPException

    assert app.exception_handlers[HTTPException] is http_exception_handler
    assert app.exception_handlers[RequestValidationError] is request_validation_exception_handler

    @app.exception_handler(Exception)
    def generic(request, exc):
        return fastrapi.responses.JSONResponse({"generic": True}, status_code=500)

    @app.get("/http")
    def http():
        raise fastrapi.HTTPException(409, "conflict")

    @app.get("/typed")
    def typed(value: int):
        return value

    assert client.get("/http").json() == {"detail": "conflict"}
    assert client.get("/typed?value=x").status_code == 422
