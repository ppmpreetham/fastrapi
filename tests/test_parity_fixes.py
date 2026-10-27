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

    flow = client.get("/openapi.json").json()["components"]["securitySchemes"]["OAuth2"]
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
