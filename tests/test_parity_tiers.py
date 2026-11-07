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


def build_strict(lib):
    from pydantic import BaseModel

    class Item(BaseModel):
        name: str

    app = lib.FastAPI(strict_content_type=False)
    strict_router = lib.APIRouter(strict_content_type=True)

    @app.post("/lenient")
    def lenient(item: Item):
        return item

    @strict_router.post("/strict")
    def strict(item: Item):
        return item

    def override(item: Item):
        return item

    strict_router.add_api_route("/override", override, methods=["POST"], strict_content_type=False)

    app.include_router(strict_router)
    return app


@pytest.mark.parametrize("url", ["/lenient", "/strict", "/override"])
def test_strict_content_type_inheritance_matches_fastapi(url):
    from fastapi.testclient import TestClient

    from conftest import LiveServerTestClient

    expected = TestClient(build_strict(fastapi)).post(url, content=b'{"name": "a"}')
    with LiveServerTestClient(build_strict(fastrapi)) as client:
        actual = client.post(url, content=b'{"name": "a"}')
    assert (actual.status_code, actual.json()) == (expected.status_code, expected.json())


def build_file(lib, path):
    app = lib.FastAPI()

    @app.api_route("/file", methods=["GET", "HEAD"])
    def file():
        return lib.responses.FileResponse(path)

    return app


def test_file_response_head_matches_fastapi(tmp_path):
    from fastapi.testclient import TestClient

    from conftest import LiveServerTestClient

    path = tmp_path / "data.txt"
    path.write_bytes(b"hello world")
    expected = TestClient(build_file(fastapi, str(path))).head("/file")
    with LiveServerTestClient(build_file(fastrapi, str(path))) as client:
        actual = client.head("/file")
    assert (actual.status_code, actual.content) == (expected.status_code, expected.content)
    assert actual.headers["content-length"] == expected.headers["content-length"]


def test_middleware_keeps_every_set_cookie(client, app):
    @app.middleware("http")
    async def tag(request, call_next):
        response = await call_next(request)
        response.headers["x-tag"] = "1"
        return response

    @app.get("/cookies")
    def cookies():
        response = fastrapi.responses.JSONResponse({})
        response.set_cookie("a", "1")
        response.set_cookie("b", "2")
        return response

    response = client.get("/cookies")
    assert response.headers["x-tag"] == "1"
    assert sorted(response.headers.get_list("set-cookie")) == ["a=1; Path=/; SameSite=lax", "b=2; Path=/; SameSite=lax"]


def test_route_and_add_websocket_route(client, app):
    from starlette.responses import PlainTextResponse

    router = fastrapi.APIRouter()

    @app.route("/plain", methods=["GET"])
    async def plain(request):
        return PlainTextResponse(request.url.path)

    @router.route("/routed")
    def routed(request):
        return PlainTextResponse("routed")

    async def echo(websocket):
        await websocket.accept()
        await websocket.close()

    app.add_websocket_route("/ws", echo)
    router.add_websocket_route("/ws2", echo)
    app.include_router(router)

    assert client.get("/plain").text == "/plain"
    assert client.get("/routed").text == "routed"
    assert "/plain" not in app.openapi()["paths"]
    assert {route.path for route in app.routes} >= {"/ws", "/ws2"}


def test_background_tasks_in_dependencies(client, app):
    import time

    done = []

    def dependency(tasks: fastrapi.BackgroundTasks):
        tasks.add_task(done.append, "dependency")
        return "dep"

    @app.get("/tasks")
    def endpoint(tasks: fastrapi.BackgroundTasks, value: str = fastrapi.Depends(dependency)):
        tasks.add_task(done.append, "endpoint")
        return {"value": value}

    @app.get("/dep-only", dependencies=[fastrapi.Depends(dependency)])
    def dep_only():
        return {}

    assert client.get("/tasks").json() == {"value": "dep"}
    assert client.get("/dep-only").status_code == 200
    for _ in range(50):
        if len(done) == 3:
            break
        time.sleep(0.02)
    assert done == ["dependency", "endpoint", "dependency"]


def build_optional_security(lib):
    security = lib.security
    app = lib.FastAPI()
    schemes = {
        "bearer": security.HTTPBearer(auto_error=False),
        "basic": security.HTTPBasic(auto_error=False),
        "header": security.APIKeyHeader(name="x-key", auto_error=False),
        "query": security.APIKeyQuery(name="key", auto_error=False),
        "cookie": security.APIKeyCookie(name="key", auto_error=False),
        "oauth2": security.OAuth2PasswordBearer(tokenUrl="token", auto_error=False),
    }
    for name, scheme in schemes.items():

        def endpoint(credentials=lib.Depends(scheme)):
            return {"missing": credentials is None}

        app.add_api_route(f"/{name}", endpoint)
    return app


@pytest.mark.parametrize("name", ["bearer", "basic", "header", "query", "cookie", "oauth2"])
def test_auto_error_false_gives_none(name):
    from fastapi.testclient import TestClient

    from conftest import LiveServerTestClient

    expected = TestClient(build_optional_security(fastapi)).get(f"/{name}")
    with LiveServerTestClient(build_optional_security(fastrapi)) as client:
        actual = client.get(f"/{name}")
    assert (actual.status_code, actual.json()) == (expected.status_code, expected.json())
