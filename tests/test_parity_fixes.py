from fastrapi import FastrAPI


def test_docs_pages_load_the_served_spec(client, app):
    @app.get("/a")
    def a():
        return {}

    docs = client.get("/docs").text
    assert "url: '/openapi.json'" in docs
    assert "<title>FastAPI - Swagger UI</title>" in docs
    assert 'spec-url="/openapi.json"' in client.get("/redoc").text


def test_swagger_oauth2_redirect_page(client, app):
    assert "oauth2RedirectUrl: window.location.origin + '/docs/oauth2-redirect'" in client.get("/docs").text
    response = client.get("/docs/oauth2-redirect")
    assert response.status_code == 200
    assert "swaggerUIRedirectOauth2" in response.text


def test_request_method(client, app):
    from fastrapi import Request

    @app.post("/m")
    def m(request: Request):
        return {"method": request.method}

    assert client.post("/m").json() == {"method": "POST"}


def test_version():
    import fastrapi

    assert isinstance(fastrapi.__version__, str)


def test_oauth2_classes_take_fastapi_argument_names():
    from fastrapi.security import OAuth2AuthorizationCodeBearer, OAuth2PasswordBearer

    assert OAuth2PasswordBearer(tokenUrl="/token").token_url == "/token"
    assert OAuth2PasswordBearer(token_url="/token").token_url == "/token"
    bearer = OAuth2AuthorizationCodeBearer(
        authorizationUrl="/auth", tokenUrl="/token", refreshUrl="/refresh"
    )
    assert (bearer.authorization_url, bearer.token_url, bearer.refresh_url) == (
        "/auth",
        "/token",
        "/refresh",
    )


def test_password_bearer_refresh_url_in_openapi(client, app):
    from fastrapi import Depends
    from fastrapi.security import OAuth2PasswordBearer

    scheme = OAuth2PasswordBearer(tokenUrl="/token", refreshUrl="/refresh")

    @app.get("/me")
    def me(token: str = Depends(scheme)):
        return {}

    flow = client.get("/openapi.json").json()["components"]["securitySchemes"]["OAuth2PasswordBearer"]
    assert flow["flows"]["password"] == {"tokenUrl": "/token", "refreshUrl": "/refresh", "scopes": {}}


def test_openid_connect_takes_fastapi_argument_name():
    from fastrapi.security import OpenIdConnect

    assert OpenIdConnect(openIdConnectUrl="/oidc").openid_connect_url == "/oidc"


def test_http_bearer_takes_fastapi_argument_name():
    from fastrapi.security import HTTPBearer

    assert HTTPBearer(bearerFormat="JWT").bearer_format == "JWT"


def test_upload_file_headers(client, app):
    from fastrapi import UploadFile

    @app.post("/upload")
    async def upload(file: UploadFile):
        return {"content_type": file.headers.get("content-type")}

    response = client.post("/upload", files={"file": ("a.txt", b"hi", "text/plain")})
    assert response.json() == {"content_type": "text/plain"}


def test_upload_file_headers_argument():
    import io

    from starlette.datastructures import Headers

    from fastrapi import UploadFile

    upload = UploadFile(io.BytesIO(b"x"), headers=Headers({"content-type": "image/png"}))
    assert upload.content_type == "image/png"
    assert upload.headers["content-type"] == "image/png"


def test_upload_file_file_attribute(client, app):
    from fastrapi import UploadFile

    @app.post("/upload-sync")
    def upload(file: UploadFile):
        return {"data": file.file.read().decode()}

    response = client.post("/upload-sync", files={"file": ("a.txt", b"hello", "text/plain")})
    assert response.json() == {"data": "hello"}


def test_background_tasks_list():
    from starlette.background import BackgroundTask

    from fastrapi import BackgroundTasks

    def job(a, b=0):
        return a + b

    tasks = BackgroundTasks(tasks=[BackgroundTask(job, 1, b=2)])
    tasks.add_task(job, 3)
    assert [(task.func, task.args, task.kwargs) for task in tasks.tasks] == [
        (job, (1,), {"b": 2}),
        (job, (3,), {}),
    ]


def test_middleware_submodules():
    from fastrapi.middleware.gzip import GZipMiddleware
    from fastrapi.middleware.httpsredirect import HTTPSRedirectMiddleware
    from fastrapi.middleware.trustedhost import TrustedHostMiddleware
    from fastrapi.middleware.wsgi import WSGIMiddleware

    assert all((GZipMiddleware, HTTPSRedirectMiddleware, TrustedHostMiddleware, WSGIMiddleware))


def test_default_placeholder():
    from fastrapi.datastructures import Default, DefaultPlaceholder

    placeholder = Default(0)
    assert isinstance(placeholder, DefaultPlaceholder)
    assert placeholder.value == 0
    assert not placeholder
    assert placeholder == Default(0)


def test_get_value_or_default_skips_placeholders():
    from fastrapi.datastructures import Default
    from fastrapi.utils import get_value_or_default

    first = Default(1)
    assert get_value_or_default(first, Default(2), None, 3) is None
    assert get_value_or_default(first, Default(2)) is first


def test_get_path_param_names_returns_a_set():
    from fastrapi.utils import get_path_param_names

    assert get_path_param_names("/a/{x}/{y}/{x}") == {"x", "y"}


def test_deep_dict_update_merges_lists():
    from fastrapi.utils import deep_dict_update

    main = {"a": {"b": 1}, "tags": ["x"]}
    assert deep_dict_update(main, {"a": {"c": 2}, "tags": ["y"]}) is None
    assert main == {"a": {"b": 1, "c": 2}, "tags": ["x", "y"]}


def test_generate_unique_id_matches_fastapi():
    from types import SimpleNamespace

    from fastrapi.utils import generate_unique_id

    route = SimpleNamespace(name="read_item", path_format="/items/{item_id}", methods={"GET"})
    assert generate_unique_id(route) == "read_item_items__item_id__get"


def test_generate_operation_id_for_path_matches_fastapi():
    import pytest

    from fastrapi.exceptions import FastAPIDeprecationWarning
    from fastrapi.utils import generate_operation_id_for_path

    with pytest.warns(FastAPIDeprecationWarning):
        operation_id = generate_operation_id_for_path(name="read", path="/items/{id}", method="GET")
    assert operation_id == "read_items__id__get"


def test_openapi_constants():
    from fastrapi.openapi.constants import METHODS_WITH_BODY, REF_PREFIX, REF_TEMPLATE

    assert METHODS_WITH_BODY == {"GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"}
    assert REF_PREFIX == "#/components/schemas/"
    assert REF_TEMPLATE == "#/components/schemas/{model}"


def test_param_types_enum():
    from fastrapi.params import ParamTypes

    assert [member.value for member in ParamTypes] == ["query", "header", "path", "cookie"]


def test_validation_error_endpoint_context():
    from fastrapi.exceptions import RequestValidationError

    exc = RequestValidationError([], body={"a": 1}, endpoint_ctx={"function": "f", "path": "/p", "line": 3})
    assert exc.body == {"a": 1}
    assert (exc.endpoint_function, exc.endpoint_path, exc.endpoint_file, exc.endpoint_line) == ("f", "/p", None, 3)


def test_exception_error_models():
    from pydantic import BaseModel

    from fastrapi.exceptions import EndpointContext, RequestErrorModel, WebSocketErrorModel

    assert EndpointContext(function="f") == {"function": "f"}
    assert issubclass(RequestErrorModel, BaseModel)
    assert WebSocketErrorModel.__name__ == "WebSocket"


def test_http_exception_subclass(client, app):
    from fastrapi import HTTPException

    class TeapotError(HTTPException):
        def __init__(self):
            super().__init__(status_code=418, detail="teapot")

    @app.get("/teapot")
    def teapot():
        raise TeapotError()

    response = client.get("/teapot")
    assert response.status_code == 418
    assert response.json() == {"detail": "teapot"}
    assert HTTPException(404, "missing").detail == "missing"


def test_cors_default_methods_are_get_only(client, app):
    from fastrapi.middleware import CORSMiddleware

    app.add_middleware(CORSMiddleware, allow_origins=["https://a.example"])

    @app.get("/c")
    def c():
        return {}

    response = client.options(
        "/c",
        headers={"Origin": "https://a.example", "Access-Control-Request-Method": "GET"},
    )
    assert response.headers["access-control-allow-methods"] == "GET"


def test_cors_wildcards_with_credentials(client, app):
    from fastrapi.middleware import CORSMiddleware

    app.add_middleware(
        CORSMiddleware,
        allow_origins=["*"],
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )

    @app.get("/c")
    def c():
        return {}

    response = client.options(
        "/c",
        headers={
            "Origin": "https://a.example",
            "Access-Control-Request-Method": "GET",
            "Access-Control-Request-Headers": "x-token",
        },
    )
    assert response.headers["access-control-allow-origin"] == "https://a.example"
    assert response.headers["access-control-allow-credentials"] == "true"
    assert response.headers["access-control-allow-headers"] == "x-token"


def test_https_redirect_uses_https(client, app):
    from fastrapi.middleware import HTTPSRedirectMiddleware

    app.add_middleware(HTTPSRedirectMiddleware)

    @app.get("/r")
    def r():
        return {}

    response = client.get("/r?x=1", follow_redirects=False)
    assert response.status_code == 307
    assert response.headers["location"].startswith("https://127.0.0.1")
    assert response.headers["location"].endswith("/r?x=1")


def test_trusted_host_redirects_to_www(client, app):
    from fastrapi.middleware import TrustedHostMiddleware

    app.add_middleware(TrustedHostMiddleware, allowed_hosts=["www.example.com"])

    @app.get("/t")
    def t():
        return {}

    response = client.get("/t", headers={"Host": "example.com"}, follow_redirects=False)
    assert response.status_code == 307
    assert response.headers["location"] == "http://www.example.com/t"
    assert client.get("/t", headers={"Host": "other.com"}).status_code == 400


def test_request_url_inside_http_middleware(client, app):
    @app.middleware("http")
    async def tag(request, call_next):
        response = await call_next(request)
        return {"path": request.url.path, "query": request.url.query, "status": response.status_code}

    @app.get("/u")
    def u():
        return {}

    assert client.get("/u?a=1").json() == {"path": "/u", "query": "a=1", "status": 200}


def test_middleware_early_return_skips_endpoint(client, app):
    import time

    from fastrapi.responses import JSONResponse

    calls = []

    @app.middleware("http")
    async def guard(request, call_next):
        if request.url.path == "/blocked":
            return JSONResponse({"blocked": True}, status_code=403)
        return await call_next(request)

    @app.get("/blocked")
    def blocked():
        calls.append("blocked")
        return {}

    @app.get("/open")
    def open_route():
        calls.append("open")
        return {"ok": True}

    assert client.get("/blocked").status_code == 403
    assert client.get("/open").json() == {"ok": True}
    time.sleep(0.2)
    assert calls == ["open"]


def test_declared_middleware_receives_kwargs():
    import socket
    import threading
    import time

    import httpx

    from fastrapi.middleware import Middleware

    class AddHeader:
        def __init__(self, app, value):
            self.app = app
            self.value = value

        async def __call__(self, scope, receive, send):
            async def send_with_header(message):
                if message["type"] == "http.response.start":
                    message["headers"] = list(message.get("headers", [])) + [(b"x-tag", self.value.encode())]
                await send(message)

            await self.app(scope, receive, send_with_header)

    app = FastrAPI(middleware=[Middleware(AddHeader, value="set")])

    @app.get("/m")
    def m():
        return {}

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    threading.Thread(target=app.serve, args=("127.0.0.1", port), daemon=True).start()
    time.sleep(0.5)
    assert httpx.get(f"http://127.0.0.1:{port}/m").headers["x-tag"] == "set"


def test_bool_query_accepts_pydantic_strings(client, app):
    @app.get("/flag")
    def flag(on: bool):
        return {"on": on}

    assert [client.get("/flag", params={"on": v}).json()["on"] for v in ("t", "Y", "f", "n")] == [True, True, False, False]


def test_max_length_counts_characters(client, app):
    from fastrapi import Query

    @app.get("/name")
    def name(value: str = Query(max_length=3)):
        return {"value": value}

    assert client.get("/name", params={"value": "äöü"}).json() == {"value": "äöü"}
    assert client.get("/name", params={"value": "äöüx"}).status_code == 422


def test_repeated_query_key_uses_last_value(client, app):
    @app.get("/last")
    def last(q: str):
        return {"q": q}

    assert client.get("/last?q=a&q=b").json() == {"q": "b"}


def test_path_params_are_percent_decoded(client, app):
    @app.get("/items/{name}")
    def item(name: str):
        return {"name": name}

    assert client.get("/items/a%20b").json() == {"name": "a b"}


def test_ellipsis_default_is_required(client, app):
    @app.get("/required")
    def required(q: int = ...):
        return {"q": q}

    assert client.get("/required", params={"q": 2}).json() == {"q": 2}
    response = client.get("/required")
    assert response.status_code == 422
    assert response.json()["detail"][0]["type"] == "missing"


def test_list_header_gets_every_value(client, app):
    from fastrapi import Header

    @app.get("/tokens")
    def tokens(x_token: list[str] = Header()):
        return {"tokens": x_token}

    response = client.get("/tokens", headers=[("x-token", "a"), ("x-token", "b")])
    assert response.json() == {"tokens": ["a", "b"]}


def test_empty_form_value_uses_default(client, app):
    from fastrapi import Form

    @app.post("/form")
    def form(name: str = Form("anon")):
        return {"name": name}

    assert client.post("/form", data={"name": ""}).json() == {"name": "anon"}


def test_float_keeps_its_fraction(client, app):
    @app.get("/float")
    def number():
        return {"value": 1.0, "big": 1e20}

    response = client.get("/float")
    assert '"value":1.0' in response.text.replace(" ", "")
    assert response.json() == {"value": 1.0, "big": 1e20}


def test_decimal_is_encoded_as_a_number(client, app):
    from decimal import Decimal

    @app.get("/decimal")
    def decimal():
        return {"whole": Decimal("3"), "fraction": Decimal("1.5")}

    assert client.get("/decimal").json() == {"whole": 3, "fraction": 1.5}


def test_openapi_success_response_uses_status_code(client, app):
    @app.post("/created", status_code=201)
    def created():
        return {}

    @app.delete("/gone", status_code=204)
    def gone():
        return None

    paths = client.get("/openapi.json").json()["paths"]
    assert "201" in paths["/created"]["post"]["responses"]
    assert "200" not in paths["/created"]["post"]["responses"]
    assert "content" not in paths["/gone"]["delete"]["responses"]["204"]


def test_openapi_response_media_type_follows_response_class(client, app):
    from fastrapi.responses import HTMLResponse

    @app.get("/page", response_class=HTMLResponse)
    def page():
        return HTMLResponse("<p>hi</p>")

    content = client.get("/openapi.json").json()["paths"]["/page"]["get"]["responses"]["200"]["content"]
    assert content == {"text/html": {"schema": {"type": "string"}}}


def test_security_credentials_can_be_created():
    from fastrapi.security import HTTPAuthorizationCredentials, HTTPBasicCredentials

    assert HTTPBasicCredentials(username="u", password="p").username == "u"
    assert HTTPAuthorizationCredentials(scheme="Bearer", credentials="t").credentials == "t"


def test_security_submodules_export_base_classes():
    from fastrapi.security.api_key import APIKeyBase
    from fastrapi.security.http import HTTPBase
    from fastrapi.security.oauth2 import SecurityScopes

    assert all((APIKeyBase, HTTPBase, SecurityScopes))


def test_concurrency_exports_contextlib_helpers():
    import contextlib

    from fastrapi import concurrency

    assert concurrency.asynccontextmanager is contextlib.asynccontextmanager
    assert concurrency.AsyncExitStack is contextlib.AsyncExitStack


def test_types_aliases():
    from fastrapi.types import DecoratedCallable, DependencyCacheKey, IncEx, ModelNameMap, UnionType

    assert DecoratedCallable.__name__ == "DecoratedCallable"
    assert all((DependencyCacheKey, IncEx, ModelNameMap, UnionType))


def test_logger_module():
    import logging

    from fastrapi.logger import logger

    assert logger is logging.getLogger("fastapi")


def test_routing_exports_starlette_helpers():
    from fastrapi.routing import Match, Route, compile_path, get_name

    assert compile_path("/items/{item_id}")[1] == "/items/{item_id}"
    assert all((Match, Route, get_name))


def test_add_event_handler(client, app):
    events = []
    app.add_event_handler("startup", lambda: events.append("started"))

    @app.get("/events")
    def read_events():
        return {"events": events}

    assert client.get("/events").json() == {"events": ["started"]}


def test_openapi_prefix_sets_root_path():
    assert FastrAPI(openapi_prefix="/api").root_path == "/api"
    assert FastrAPI(openapi_prefix="/api", root_path="/v1").root_path == "/v1"


def test_openapi_version_attribute(client, app):
    assert app.openapi_version == "3.1.0"
    app.openapi_version = "3.0.2"
    assert app.openapi()["openapi"] == "3.0.2"


def test_app_keeps_extra_keyword_arguments():
    assert FastrAPI(team="core").extra == {"team": "core"}
    assert FastrAPI().extra == {}


def test_http_exception_default_detail_uses_http_status_phrase():
    import http

    from fastrapi import HTTPException

    assert HTTPException(413).detail == http.HTTPStatus(413).phrase
    assert HTTPException(422).detail == http.HTTPStatus(422).phrase


def test_starlette_http_exception_handler_catches_http_exception(client, app):
    from starlette.exceptions import HTTPException as StarletteHTTPException

    from fastrapi import HTTPException
    from fastrapi.responses import JSONResponse

    @app.exception_handler(StarletteHTTPException)
    def handler(request, exc):
        return JSONResponse({"handled": exc.status_code}, status_code=exc.status_code)

    @app.get("/missing")
    def missing():
        raise HTTPException(404)

    response = client.get("/missing")
    assert response.status_code == 404
    assert response.json() == {"handled": 404}


def test_raised_starlette_http_exception_becomes_a_response(client, app):
    from starlette.exceptions import HTTPException as StarletteHTTPException

    @app.get("/teapot-starlette")
    def teapot():
        raise StarletteHTTPException(status_code=418, detail="short and stout")

    response = client.get("/teapot-starlette")
    assert response.status_code == 418
    assert response.json() == {"detail": "short and stout"}


def test_html_response_class_with_string_return(client, app):
    from fastrapi.responses import HTMLResponse

    @app.get("/html-text", response_class=HTMLResponse)
    def html_text():
        return "<h1>hi</h1>"

    response = client.get("/html-text")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/html")
    assert response.text == "<h1>hi</h1>"


def test_redirect_response_class_with_string_return(client, app):
    from fastrapi.responses import RedirectResponse

    @app.get("/go", response_class=RedirectResponse)
    def go():
        return "https://example.com/target"

    @app.get("/go-permanent", response_class=RedirectResponse, status_code=301)
    def go_permanent():
        return "/elsewhere"

    response = client.get("/go", follow_redirects=False)
    assert response.status_code == 307
    assert response.headers["location"] == "https://example.com/target"
    response = client.get("/go-permanent", follow_redirects=False)
    assert response.status_code == 301
    assert response.headers["location"] == "/elsewhere"


def test_file_response_class_with_path_return(client, app, tmp_path):
    from fastrapi.responses import FileResponse

    target = tmp_path / "hello.txt"
    target.write_text("hello file")

    @app.get("/file-path", response_class=FileResponse)
    def file_path():
        return str(target)

    @app.get("/file-pathlib", response_class=FileResponse)
    def file_pathlib():
        return target

    assert client.get("/file-path").text == "hello file"
    assert client.get("/file-pathlib").text == "hello file"


def test_file_response_content_disposition_type(client, app, tmp_path):
    from fastrapi.responses import FileResponse

    target = tmp_path / "report.txt"
    target.write_text("data")

    @app.get("/inline")
    def inline():
        return FileResponse(target, filename="report.txt", content_disposition_type="inline")

    response = client.get("/inline")
    assert response.headers["content-disposition"] == 'inline; filename="report.txt"'


def test_scalar_validation_errors_carry_ctx(client, app):
    from fastrapi import Query

    @app.get("/limits")
    def limits(n: int = Query(gt=0), s: str = Query("abc", min_length=3)):
        return {}

    error = client.get("/limits", params={"n": 0}).json()["detail"][0]
    assert error["ctx"] == {"gt": 0}
    error = client.get("/limits", params={"n": 1, "s": "a"}).json()["detail"][0]
    assert error["type"] == "string_too_short"
    assert error["ctx"] == {"min_length": 3}
    assert error["msg"] == "String should have at least 3 characters"


def test_invalid_json_error_shape(client, app):
    from pydantic import BaseModel

    class Item(BaseModel):
        name: str

    class Owner(BaseModel):
        email: str

    @app.post("/items-json")
    def create(item: Item, owner: Owner):
        return item

    response = client.post(
        "/items-json", content=b'{"name": ', headers={"content-type": "application/json"}
    )
    error = response.json()["detail"][0]
    assert response.status_code == 422
    assert error["type"] == "json_invalid"
    assert error["loc"][0] == "body" and isinstance(error["loc"][1], int)
    assert error["input"] == {}
    assert "error" in error["ctx"]


def test_missing_field_among_body_params(client, app):
    from pydantic import BaseModel

    class Item(BaseModel):
        name: str

    class User(BaseModel):
        username: str

    @app.post("/pair")
    def pair(item: Item, user: User):
        return {}

    response = client.post("/pair", json={"item": {"name": "x"}})
    assert response.status_code == 422
    assert response.json()["detail"][0] == {
        "type": "missing",
        "loc": ["body", "user"],
        "msg": "Field required",
        "input": None,
    }


def test_quoted_cookie_values_are_unquoted(client, app):
    from fastrapi import Cookie

    @app.get("/cookie")
    def cookie(session: str = Cookie()):
        return {"session": session}

    response = client.get("/cookie", headers={"cookie": r'session="a b\073c"'})
    assert response.json() == {"session": "a b;c"}


def test_async_partial_and_callable_endpoints(client, app):
    import functools

    async def greet(name):
        return {"hello": name}

    class Handler:
        async def __call__(self):
            return {"from": "callable"}

    app.add_api_route("/partial", functools.partial(greet, "world"), methods=["GET"])
    app.add_api_route("/callable", Handler(), methods=["GET"])

    assert client.get("/partial").json() == {"hello": "world"}
    assert client.get("/callable").json() == {"from": "callable"}


def test_class_dependency_with_async_call_is_instantiated(client, app):
    from fastrapi import Depends

    class Settings:
        def __init__(self):
            self.name = "prod"

        async def __call__(self):
            return "not used"

    @app.get("/settings")
    def settings(value: Settings = Depends(Settings)):
        return {"name": value.name}

    assert client.get("/settings").json() == {"name": "prod"}


def test_add_middleware_passes_positional_arguments(client, app):
    class Tag:
        def __init__(self, app, value):
            self.app = app
            self.value = value

        async def __call__(self, scope, receive, send):
            async def send_with_header(message):
                if message["type"] == "http.response.start":
                    message["headers"] = list(message.get("headers", [])) + [(b"x-tag", self.value.encode())]
                await send(message)

            await self.app(scope, receive, send_with_header)

    app.add_middleware(Tag, "positional")

    @app.get("/tagged")
    def tagged():
        return {}

    assert client.get("/tagged").headers["x-tag"] == "positional"


def test_websocket_iter_bytes():
    import base64
    import os
    import socket

    from test_websocket_smoke import _free_port, _recv_frame, _serve

    app = FastrAPI()

    @app.websocket("/bytes")
    async def echo_bytes(websocket):
        await websocket.accept()
        async for chunk in websocket.iter_bytes():
            await websocket.send_bytes(chunk[::-1])

    port = _free_port()
    _serve(app, port)
    key = base64.b64encode(os.urandom(16)).decode()
    with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
        sock.sendall(
            (
                "GET /bytes HTTP/1.1\r\n"
                f"Host: 127.0.0.1:{port}\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        assert b" 101 " in sock.recv(4096)
        payload = b"abc"
        mask = os.urandom(4)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        sock.sendall(bytes([0x82, 0x80 | len(payload)]) + mask + masked)
        assert _recv_frame(sock) == (0x2, b"cba")


def test_request_subclasses_and_starlette_request_are_injected(client, app):
    from starlette.requests import Request as StarletteRequest

    from fastrapi import Request

    class MyRequest(Request):
        pass

    @app.get("/sub")
    def sub(request: MyRequest):
        return {"path": request.url.path}

    @app.get("/starlette")
    def starlette_request(request: StarletteRequest):
        return {"path": request.url.path}

    assert client.get("/sub").json() == {"path": "/sub"}
    assert client.get("/starlette").json() == {"path": "/starlette"}


def test_request_url_inside_endpoint(client, app):
    from fastrapi import Request

    @app.get("/where")
    def where(request: Request):
        return {"url": str(request.url)}

    response = client.get("/where?x=1")
    assert response.json()["url"].endswith("/where?x=1")
    assert response.json()["url"].startswith("http://127.0.0.1")


def test_dependency_param_named_like_request(client, app):
    from enum import Enum

    from fastrapi import Depends, Request

    class RequestKind(str, Enum):
        read = "read"

    def current(kind: RequestKind, request: Request):
        return {"kind": getattr(kind, "value", kind), "path": request.url.path}

    @app.get("/kind")
    def kind(data: dict = Depends(current)):
        return data

    assert client.get("/kind?kind=read").json() == {"kind": "read", "path": "/kind"}


def test_str_return_annotation_is_json(client, app):
    @app.get("/hi")
    def hi() -> str:
        return "hi"

    response = client.get("/hi")
    assert response.headers["content-type"] == "application/json"
    assert response.json() == "hi"


def test_none_result_is_json_null(client, app):
    @app.get("/nothing")
    def nothing():
        return None

    @app.post("/created", status_code=201)
    def created():
        return None

    response = client.get("/nothing")
    assert response.status_code == 200
    assert response.content == b"null"
    response = client.post("/created")
    assert response.status_code == 201
    assert response.json() is None


def test_no_content_status_strips_body(client, app):
    @app.delete("/gone-body", status_code=204)
    def gone():
        return {"deleted": True}

    response = client.delete("/gone-body")
    assert response.status_code == 204
    assert response.content == b""
    assert response.headers.get("content-length") in (None, "0")


def test_generators_and_deques_are_lists(client, app):
    from collections import deque

    @app.get("/seq")
    def seq():
        return {"gen": (n * 2 for n in range(3)), "dq": deque([1, 2])}

    assert client.get("/seq").json() == {"gen": [0, 2, 4], "dq": [1, 2]}


def test_plain_objects_encode_their_fields(client, app):
    class Point:
        def __init__(self):
            self.x = 1
            self.y = 2

    class Pairs:
        def __iter__(self):
            return iter([("a", 1)])

    @app.get("/objects")
    def objects():
        return {"point": Point(), "pairs": Pairs()}

    assert client.get("/objects").json() == {"point": {"x": 1, "y": 2}, "pairs": {"a": 1}}


def test_app_accepts_custom_attributes():
    app = FastrAPI()
    app.db_pool = "pool"
    assert app.db_pool == "pool"


def test_openapi_override_is_served(client, app):
    def custom_openapi():
        return {"openapi": "3.1.0", "info": {"title": "custom", "version": "1"}, "paths": {}}

    app.openapi = custom_openapi
    assert app.openapi()["info"]["title"] == "custom"
    assert client.get("/openapi.json").json()["info"]["title"] == "custom"


def test_openapi_schema_attribute(client, app):
    @app.get("/s")
    def s():
        return {}

    assert app.openapi_schema is None
    schema = app.openapi()
    assert app.openapi_schema == schema
    app.openapi_schema = {"openapi": "3.1.0", "info": {"title": "set", "version": "1"}, "paths": {}}
    assert app.openapi()["info"]["title"] == "set"
    assert client.get("/openapi.json").json()["info"]["title"] == "set"


def test_contextmanager_in_threadpool():
    import asyncio
    from contextlib import contextmanager

    from fastrapi.concurrency import contextmanager_in_threadpool

    events = []

    @contextmanager
    def resource():
        events.append("enter")
        yield "value"
        events.append("exit")

    async def main():
        async with contextmanager_in_threadpool(resource()) as value:
            events.append(value)

    asyncio.run(main())
    assert events == ["enter", "value", "exit"]


def test_encoder_tables():
    import datetime
    from decimal import Decimal

    from fastrapi.encoders import (
        ENCODERS_BY_TYPE,
        decimal_encoder,
        encoders_by_class_tuples,
        generate_encoders_by_class_tuples,
        isoformat,
    )

    assert ENCODERS_BY_TYPE[bytes](b"x") == "x"
    assert decimal_encoder(Decimal("2")) == 2
    assert decimal_encoder(Decimal("2.5")) == 2.5
    assert isoformat(datetime.date(2024, 1, 2)) == "2024-01-02"
    assert set in encoders_by_class_tuples[list]
    assert generate_encoders_by_class_tuples({int: str})[str] == (int,)


def test_params_param_base():
    from fastrapi.params import Body, Cookie, Header, Param, Path, Query

    for cls in (Query, Header, Path, Cookie):
        assert issubclass(cls, Param)
    assert isinstance(Query(), Param)
    assert not issubclass(Body, Param)


def test_security_classes_share_bases():
    from fastrapi.security import APIKeyHeader, HTTPBearer, OAuth2PasswordBearer
    from fastrapi.security.api_key import APIKeyBase
    from fastrapi.security.base import SecurityBase
    from fastrapi.security.http import HTTPBase

    assert isinstance(APIKeyHeader(name="x-key"), APIKeyBase)
    assert isinstance(HTTPBearer(), HTTPBase)
    assert isinstance(OAuth2PasswordBearer(tokenUrl="token"), SecurityBase)
    assert issubclass(APIKeyBase, SecurityBase)


def test_https_redirect_ignores_forwarded_proto(client, app):
    from fastrapi.middleware import HTTPSRedirectMiddleware

    app.add_middleware(HTTPSRedirectMiddleware)

    @app.get("/secure-only")
    def secure_only():
        return {}

    response = client.get(
        "/secure-only", headers={"X-Forwarded-Proto": "https"}, follow_redirects=False
    )
    assert response.status_code == 307


def test_sync_generator_lifespan(client, app):
    events = []

    def lifespan(app):
        events.append("enter")
        yield

    app.lifespan = lifespan

    @app.get("/life")
    def life():
        return {"events": events}

    assert client.get("/life").json() == {"events": ["enter"]}


def test_api_route_registers_each_method(client, app):
    from fastrapi import APIRouter

    @app.api_route("/multi", methods=["GET", "POST"])
    def multi():
        return {"ok": True}

    router = APIRouter()

    @router.api_route("/routed", methods=["PUT"])
    def routed():
        return {"routed": True}

    app.include_router(router)
    assert client.get("/multi").json() == {"ok": True}
    assert client.post("/multi").json() == {"ok": True}
    assert client.put("/routed").json() == {"routed": True}


def test_response_background_replaces_injected_tasks(client, app):
    import time

    from fastrapi import BackgroundTasks
    from starlette.background import BackgroundTask
    from fastrapi.responses import JSONResponse

    ran = []

    @app.get("/bg-own")
    def bg_own(tasks: BackgroundTasks):
        tasks.add_task(ran.append, "injected")
        return JSONResponse({"ok": True}, background=BackgroundTask(ran.append, "own"))

    assert client.get("/bg-own").json() == {"ok": True}
    time.sleep(0.3)
    assert ran == ["own"]


def test_async_exit_stack_middleware():
    import asyncio
    from contextlib import AsyncExitStack

    from fastrapi.middleware.asyncexitstack import AsyncExitStackMiddleware

    seen = {}

    async def inner(scope, receive, send):
        seen["stack"] = scope["fastapi_middleware_astack"]

    asyncio.run(AsyncExitStackMiddleware(inner)({}, None, None))
    assert isinstance(seen["stack"], AsyncExitStack)


def test_cors_origin_regex(client, app):
    from fastrapi.middleware import CORSMiddleware

    app.add_middleware(CORSMiddleware, allow_origin_regex="https://[a-z]+[.]example[.]org")

    @app.get("/rx")
    def rx():
        return {}

    allowed = client.get("/rx", headers={"Origin": "https://app.example.org"})
    assert allowed.headers["access-control-allow-origin"] == "https://app.example.org"
    denied = client.get("/rx", headers={"Origin": "https://evil.example.com"})
    assert "access-control-allow-origin" not in denied.headers


def test_url_path_for_named_routes():
    import pytest
    from starlette.routing import NoMatchFound

    from fastrapi import APIRouter

    app = FastrAPI()
    router = APIRouter(prefix="/users")

    @router.get("/{user_id}", name="read_user")
    def read_user(user_id: int):
        return {}

    @app.get("/items/{item_id}")
    def read_item(item_id: int):
        return {}

    app.include_router(router)
    assert app.url_path_for("read_item", item_id=3) == "/items/3"
    assert app.url_path_for("read_user", user_id=7) == "/users/7"
    assert router.url_path_for("read_user", user_id=7) == "/users/7"
    with pytest.raises(NoMatchFound):
        app.url_path_for("read_item")


def test_openapi_documents_head_and_options(client, app):
    @app.head("/probe")
    def probe_head():
        return None

    @app.options("/probe")
    def probe_options():
        return {}

    paths = client.get("/openapi.json").json()["paths"]
    assert paths["/probe"]["head"]["operationId"] == "probe_head_probe_head"
    assert paths["/probe"]["options"]["operationId"] == "probe_options_probe_options"


def test_openapi_security_requirements_are_alternatives(client, app):
    from fastrapi import APIRouter, Depends
    from fastrapi.security import APIKeyHeader, HTTPBearer

    key = APIKeyHeader(name="x-key", auto_error=False)
    bearer = HTTPBearer(auto_error=False)
    router = APIRouter(dependencies=[Depends(key)])

    @router.get("/both")
    def both(token=Depends(bearer)):
        return {}

    app.include_router(router)
    spec = client.get("/openapi.json").json()
    assert spec["paths"]["/both"]["get"]["security"] == [{"APIKeyHeader": []}, {"HTTPBearer": []}]


def test_openapi_documents_dependency_parameters(client, app):
    from typing import Annotated

    from fastrapi import Depends, Header, Query

    def paging(limit: int = 10, x_tenant: Annotated[str, Header()] = "a"):
        return limit

    @app.get("/paged")
    def paged(page: int = Depends(paging), q: Annotated[str | None, Query()] = None):
        return {}

    params = client.get("/openapi.json").json()["paths"]["/paged"]["get"]["parameters"]
    assert [(p["in"], p["name"]) for p in params] == [("query", "limit"), ("header", "x-tenant"), ("query", "q")]


def test_openapi_operation_ids_use_names_and_route_objects(client, app):
    from fastrapi import APIRouter

    def tagged_id(route):
        return f"{route.tags[0]}-{route.name}-{route.path_format}"

    router = APIRouter(prefix="/r", tags=["things"], generate_unique_id_function=tagged_id)

    @router.get("/{thing_id}")
    def read_thing(thing_id: int):
        return {}

    @app.get("/named", name="custom_name")
    def named():
        return {}

    app.include_router(router)
    paths = client.get("/openapi.json").json()["paths"]
    assert paths["/r/{thing_id}"]["get"]["operationId"] == "things-read_thing-/r/{thing_id}"
    assert paths["/named"]["get"]["operationId"] == "custom_name_named_get"
    assert paths["/named"]["get"]["summary"] == "Custom Name"


def test_openapi_separate_input_output_schemas(client, app):
    from pydantic import BaseModel, computed_field

    class Child(BaseModel):
        value: int

    class Item(BaseModel):
        name: str
        child: Child

        @computed_field
        @property
        def upper(self) -> str:
            return self.name.upper()

    @app.post("/items", response_model=Item)
    def create(item: Item):
        return item

    @app.get("/items", response_model=list[Item])
    def listing():
        return []

    spec = client.get("/openapi.json").json()
    schemas = spec["components"]["schemas"]
    assert {"Item-Input", "Item-Output", "Child"} <= set(schemas)
    post = spec["paths"]["/items"]["post"]
    assert post["requestBody"]["content"]["application/json"]["schema"] == {"$ref": "#/components/schemas/Item-Input"}
    assert post["responses"]["200"]["content"]["application/json"]["schema"] == {"$ref": "#/components/schemas/Item-Output"}
    listed = spec["paths"]["/items"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
    assert listed == {"type": "array", "items": {"$ref": "#/components/schemas/Item-Output"}, "title": "Response Listing Items Get"}
    assert schemas["Item-Input"]["properties"]["child"] == {"$ref": "#/components/schemas/Child"}


def test_openapi_request_bodies_for_forms_files_and_plain_values(client, app):
    from typing import Annotated

    from fastrapi import Body, File, Form, UploadFile

    @app.post("/login")
    def login(username: Annotated[str, Form()], password: Annotated[str, Form()]):
        return {}

    @app.post("/upload")
    def upload(file: UploadFile = File(...), note: Annotated[str | None, Form()] = None):
        return {}

    @app.put("/count")
    def count(value: Annotated[int, Body()]):
        return {}

    @app.post("/optional")
    def optional(value: Annotated[int | None, Body()] = None):
        return {}

    spec = client.get("/openapi.json").json()
    paths, schemas = spec["paths"], spec["components"]["schemas"]
    login_body = paths["/login"]["post"]["requestBody"]
    assert login_body["required"] is True
    assert login_body["content"] == {"application/x-www-form-urlencoded": {"schema": {"$ref": "#/components/schemas/Body_login_login_post"}}}
    assert schemas["Body_login_login_post"]["required"] == ["username", "password"]
    upload_body = paths["/upload"]["post"]["requestBody"]["content"]
    assert list(upload_body) == ["multipart/form-data"]
    assert schemas["Body_upload_upload_post"]["properties"]["file"] == {"type": "string", "format": "binary", "title": "File"}
    assert paths["/count"]["put"]["requestBody"] == {"content": {"application/json": {"schema": {"type": "integer", "title": "Value"}}}, "required": True}
    assert "required" not in paths["/optional"]["post"]["requestBody"]


def test_openapi_additional_response_models(client, app):
    from pydantic import BaseModel

    from fastrapi import APIRouter

    class Missing(BaseModel):
        detail: str

    router = APIRouter(responses={418: {"description": "Teapot"}})

    @router.get("/things/{thing_id}", responses={404: {"model": Missing}, 500: {}})
    def thing(thing_id: int):
        return {}

    app.include_router(router, responses={403: {"description": "Forbidden here"}})
    spec = client.get("/openapi.json").json()
    responses = spec["paths"]["/things/{thing_id}"]["get"]["responses"]
    assert responses["404"] == {
        "description": "Not Found",
        "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Missing"}}},
    }
    assert responses["500"] == {"description": "Internal Server Error"}
    assert responses["418"]["description"] == "Teapot"
    assert responses["403"]["description"] == "Forbidden here"
    assert "Missing" in spec["components"]["schemas"]


def test_get_openapi_with_app_routes(client, app):
    from pydantic import BaseModel

    from fastrapi.openapi.utils import get_openapi
    from fastrapi.routing import APIRoute

    class Item(BaseModel):
        name: str

    @app.post("/items", response_model=Item, tags=["items"])
    def create_item(item: Item, verbose: bool = False):
        return item

    for route in app.routes:
        if isinstance(route, APIRoute):
            route.operation_id = route.name

    def custom_openapi():
        if app.openapi_schema:
            return app.openapi_schema
        schema = get_openapi(title="Custom", version="2.0", routes=app.routes)
        schema["info"]["x-logo"] = {"url": "logo.png"}
        app.openapi_schema = schema
        return schema

    app.openapi = custom_openapi
    spec = client.get("/openapi.json").json()
    operation = spec["paths"]["/items"]["post"]
    assert spec["info"] == {"title": "Custom", "version": "2.0", "x-logo": {"url": "logo.png"}}
    assert operation["operationId"] == "create_item"
    assert operation["parameters"][0]["name"] == "verbose"
    assert operation["requestBody"]["content"]["application/json"]["schema"] == {"$ref": "#/components/schemas/Item"}
    assert "Item" in spec["components"]["schemas"]
    assert [route.path for route in app.routes] == ["/items"]


def test_openapi_models_match_fastapi():
    from fastrapi.openapi.models import (
        OAuth2,
        OAuthFlowPassword,
        OAuthFlows,
        OpenAPI,
        Operation,
        Reference,
        Schema,
    )

    spec = OpenAPI.model_validate(
        {
            "openapi": "3.1.0",
            "info": {"title": "T", "version": "1"},
            "paths": {"/a": {"get": {"responses": {"200": {"description": "ok"}}}}},
        }
    )
    assert isinstance(spec.paths["/a"].get, Operation)
    assert Reference(**{"$ref": "#/x"}).ref == "#/x"
    assert Schema(type="string").type == "string"
    flows = OAuthFlows(password=OAuthFlowPassword(tokenUrl="token"))
    assert OAuth2(flows=flows).type_.value == "oauth2"


def test_openapi_utils_helpers():
    from fastrapi.openapi.utils import (
        generate_operation_summary,
        get_openapi_operation_metadata,
        get_openapi_path,
        status_code_ranges,
        validation_error_response_definition,
    )
    from fastrapi.routing import APIRoute

    def read_item(item_id: int):
        return {}

    route = APIRoute("/items/{item_id}", read_item, methods=["GET"], tags=["items"])
    assert status_code_ranges["4XX"] == "Client Error"
    assert validation_error_response_definition["title"] == "HTTPValidationError"
    assert generate_operation_summary(route=route, method="GET") == "Read Item"
    ids = set()
    metadata = get_openapi_operation_metadata(route=route, method="GET", operation_ids=ids)
    assert metadata["operationId"] == "read_item_items__item_id__get"
    path, security, schemas = get_openapi_path(route=route, operation_ids=set())
    assert path["get"]["parameters"][0]["name"] == "item_id"
    assert "HTTPValidationError" in schemas


def test_upload_file_headers_include_all_part_headers(client, app):
    from fastrapi import UploadFile

    @app.post("/up")
    async def up(file: UploadFile):
        return {"disposition": file.headers["content-disposition"], "extra": file.headers.get("x-extra")}

    body = (
        b"--b\r\n"
        b'Content-Disposition: form-data; name="file"; filename="a.txt"\r\n'
        b"Content-Type: text/plain\r\n"
        b"X-Extra: yes\r\n\r\n"
        b"hello\r\n--b--\r\n"
    )
    response = client.post("/up", content=body, headers={"content-type": "multipart/form-data; boundary=b"})
    assert response.json() == {"disposition": 'form-data; name="file"; filename="a.txt"', "extra": "yes"}


def test_method_not_allowed_uses_handler(client, app):
    from fastrapi.responses import JSONResponse

    @app.exception_handler(405)
    async def not_allowed(request, exc):
        return JSONResponse({"custom": exc.status_code}, status_code=405)

    @app.get("/only-get")
    def only_get():
        return {}

    response = client.post("/only-get")
    assert response.status_code == 405
    assert response.json() == {"custom": 405}
    assert response.headers["allow"] == "GET"


def test_root_path_routes_paths_outside_the_prefix():
    import httpx
    from test_websocket_smoke import _free_port, _serve

    app = FastrAPI(root_path="/api")

    @app.get("/items")
    def items():
        return {"ok": True}

    port = _free_port()
    _serve(app, port)
    assert httpx.get(f"http://127.0.0.1:{port}/api/items").json() == {"ok": True}
    assert httpx.get(f"http://127.0.0.1:{port}/items").json() == {"ok": True}


def test_head_on_get_route_is_method_not_allowed(client, app):
    @app.get("/plain")
    def plain():
        return {}

    @app.get("/both")
    def both_get():
        return {"get": True}

    @app.head("/both")
    def both_head():
        return None

    response = client.head("/plain")
    assert response.status_code == 405
    assert response.headers["allow"] == "GET"
    assert client.head("/both").status_code == 200


def test_trailing_slash_routes_and_redirects():
    import httpx
    from test_websocket_smoke import _free_port, _serve

    app = FastrAPI()

    @app.get("/items/")
    def items():
        return {"slash": True}

    @app.get("/plain")
    def plain():
        return {"slash": False}

    @app.get("/plain/")
    def plain_slash():
        return {"slash": True}

    port = _free_port()
    _serve(app, port)
    base = f"http://127.0.0.1:{port}"
    redirect = httpx.get(f"{base}/items?x=1")
    assert redirect.status_code == 307
    assert redirect.headers["location"] == f"{base}/items/?x=1"
    assert httpx.get(f"{base}/items/").json() == {"slash": True}
    assert httpx.get(f"{base}/plain").json() == {"slash": False}
    assert httpx.get(f"{base}/plain/").json() == {"slash": True}

    strict = FastrAPI(redirect_slashes=False)

    @strict.get("/only/")
    def only():
        return {}

    strict_port = _free_port()
    _serve(strict, strict_port)
    assert httpx.get(f"http://127.0.0.1:{strict_port}/only").status_code == 404


def test_add_api_route_accepts_fastapi_options(client, app):
    import pytest
    from pydantic import BaseModel

    from fastrapi import APIRouter
    from fastrapi.routing import APIRoute

    class Out(BaseModel):
        name: str
        secret: str | None = None

    class TaggedRoute(APIRoute):
        pass

    def read():
        return {"name": "a"}

    router = APIRouter()
    router.add_api_route(
        "/r",
        read,
        response_model=Out,
        response_model_exclude_none=True,
        route_class_override=TaggedRoute,
        strict_content_type=False,
    )
    app.add_api_route("/a", read, response_model=Out, response_model_exclude_unset=True)
    app.include_router(router)
    with pytest.raises(TypeError):
        app.add_api_route("/bad", read, not_an_option=1)
    assert client.get("/r").json() == {"name": "a"}
    assert client.get("/a").json() == {"name": "a"}


def test_router_trace_add_route_and_websocket_helpers(client, app):
    from fastrapi import APIRouter
    from fastrapi.responses import PlainTextResponse

    router = APIRouter()

    @router.trace("/traced")
    def traced():
        return {"trace": True}

    def starlette_style(request):
        return PlainTextResponse(f"{request.method} {request.url.path}")

    async def socket(websocket):
        await websocket.accept()
        await websocket.close()

    router.add_route("/plain", starlette_style, methods=["GET"])
    router.add_api_websocket_route("/ws-added", socket)
    app.include_router(router)

    @app.websocket_route("/ws-route")
    async def ws_route(websocket):
        await websocket.accept()
        await websocket.close()

    assert client.request("TRACE", "/traced").json() == {"trace": True}
    assert client.get("/plain").text == "GET /plain"
    spec = client.get("/openapi.json").json()
    assert "trace" in spec["paths"]["/traced"]
    assert "/plain" not in spec["paths"]
    assert {route.path for route in app.routes if not hasattr(route, "methods")} == {"/ws-route", "/ws-added"}


def test_router_events_and_lifespans_merge(client, app):
    from contextlib import asynccontextmanager

    from fastrapi import APIRouter

    events = []

    @asynccontextmanager
    async def router_lifespan(app):
        events.append("router-lifespan")
        yield {"from_router": True}

    router = APIRouter(on_startup=[lambda: events.append("ctor-startup")], lifespan=router_lifespan)

    @router.on_event("startup")
    def on_start():
        events.append("decorated-startup")

    router.add_event_handler("startup", lambda: events.append("added-startup"))
    assert callable(router.lifespan_context)

    @router.get("/events")
    def read_events():
        return events

    app.include_router(router)
    assert sorted(client.get("/events").json()) == sorted(
        ["router-lifespan", "ctor-startup", "decorated-startup", "added-startup"]
    )


def test_router_mount_and_host(tmp_path):
    import httpx
    from test_websocket_smoke import _free_port, _serve

    from fastrapi import APIRouter
    from fastrapi.staticfiles import StaticFiles

    (tmp_path / "hello.txt").write_text("hi there")
    app = FastrAPI()
    router = APIRouter(prefix="/assets")
    router.mount("/static", StaticFiles(directory=str(tmp_path)), name="static")
    sub = FastrAPI()

    @sub.get("/sub")
    def sub_route():
        return {"host": True}

    router.host("api.example.test", sub)
    app.include_router(router)
    port = _free_port()
    _serve(app, port)
    assert httpx.get(f"http://127.0.0.1:{port}/assets/static/hello.txt").text == "hi there"
    hosted = httpx.get(f"http://127.0.0.1:{port}/sub", headers={"host": "api.example.test"})
    assert hosted.json() == {"host": True}
