import asyncio
import contextlib
import json
import base64
import os
import socket
import threading
import time
from typing import Annotated, Literal

import fastapi
import fastapi.datastructures
import fastapi.routing
import fastapi.security
import pytest
from fastapi.testclient import TestClient
from pydantic import AliasChoices, BaseModel, Field
from starlette.background import BackgroundTask

import fastrapi
import fastrapi.datastructures
import fastrapi.routing
import fastrapi.security
from test_security_ws_parity import LiveServer


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def both(run):
    assert run(fastrapi) == run(fastapi)


def test_parameter_bounds_keep_python_number_types():
    def run(lib):
        app = new_app(lib)

        @app.get("/bounds")
        def bounds(a: int = lib.Query(gt=3, le=10), b: float = lib.Query(ge=0.0, lt=7)):
            return a

        parameters = app.openapi()["paths"]["/bounds"]["get"]["parameters"]
        return [{key: (value, type(value).__name__) for key, value in p["schema"].items() if "imum" in key} for p in parameters]

    both(run)


def test_background_tasks_run_in_order_and_stop_at_first_failure():
    def run(lib):
        log = []
        app = new_app(lib)

        async def slow(tag):
            await asyncio.sleep(0.02)
            log.append(tag)

        def sync(tag):
            log.append((tag, threading.current_thread() is threading.main_thread()))

        def boom():
            raise RuntimeError("boom")

        @app.get("/order")
        def order(tasks: lib.BackgroundTasks):
            tasks.add_task(slow, "a1")
            tasks.add_task(sync, "s1")
            tasks.add_task(slow, "a2")
            tasks.add_task(boom)
            tasks.add_task(sync, "never")
            return "ok"

        @app.get("/response")
        def response():
            return lib.responses.JSONResponse("ok", background=BackgroundTask(slow, "single"))

        @app.get("/call")
        async def call():
            tasks = lib.BackgroundTasks()
            tasks.add_task(slow, "c1")
            tasks.add_task(sync, "c2")
            await tasks()
            return [task.is_async for task in tasks.tasks]

        client = TestClient(app, raise_server_exceptions=False)
        bodies = [client.get(path).json() for path in ("/order", "/response", "/call")]
        time.sleep(0.2)
        return bodies, log

    both(run)


def test_get_websocket_app_solves_dependencies():
    from starlette.applications import Starlette
    from starlette.routing import WebSocketRoute
    from starlette.testclient import TestClient as StarletteClient
    from starlette.websockets import WebSocketDisconnect

    from fastrapi.dependencies.utils import get_dependant
    from fastrapi.routing import get_websocket_app

    def prefix():
        return "hi"

    async def endpoint(websocket: fastrapi.WebSocket, name: str, greeting: str = fastrapi.Depends(prefix)):
        await websocket.accept()
        await websocket.send_text(f"{greeting} {name}")
        await websocket.close()

    app = get_websocket_app(get_dependant(path="/ws", call=endpoint))
    client = StarletteClient(Starlette(routes=[WebSocketRoute("/ws", app)]))
    with client.websocket_connect("/ws?name=bob") as websocket:
        assert websocket.receive_text() == "hi bob"
    try:
        with client.websocket_connect("/ws") as websocket:
            websocket.receive_text()
    except Exception as exc:
        assert isinstance(exc, (WebSocketDisconnect, fastrapi.exceptions.WebSocketRequestValidationError))
    else:
        raise AssertionError("missing query parameter was accepted")


def test_route_dependencies_keep_marker_options():
    def run(lib):
        events = []

        def function_session():
            yield
            events.append("function cleanup")

        def request_session():
            yield
            events.append("request cleanup")

        def counted():
            events.append("counted")

        def scoped(security_scopes: lib.security.SecurityScopes):
            events.append(security_scopes.scopes)

        router = lib.APIRouter(dependencies=[lib.Depends(function_session, scope="function")])

        @router.get(
            "/stream",
            dependencies=[
                lib.Depends(request_session),
                lib.Depends(counted, use_cache=False),
                lib.Security(scoped, scopes=["items"]),
            ],
        )
        def stream(again=lib.Depends(counted), fresh=lib.Depends(counted, use_cache=False)):
            def chunks():
                yield "chunk"

            return lib.responses.StreamingResponse(chunks())

        app = new_app(lib)
        app.include_router(router)
        body = TestClient(app).get("/stream").text
        return body, events

    both(run)


class Cat(BaseModel):
    kind: Literal["cat"]
    meow: str


class Dog(BaseModel):
    kind: Literal["dog"]
    bark: str
    tags: list[str] = []


class Profile(BaseModel):
    name: str
    age: int = 1


class Extra(BaseModel):
    note: str


def form_app(lib):
    app = new_app(lib)

    @app.post("/union")
    def union(pet: Annotated[Cat | Dog, lib.Form()]):
        return pet

    @app.post("/optional-union")
    def optional_union(pet: Annotated[Cat | Dog | None, lib.Form()] = None):
        return pet

    @app.post("/optional")
    def optional(profile: Annotated[Profile | None, lib.Form()] = None):
        return profile

    @app.post("/combined")
    def combined(profile: Annotated[Profile, lib.Form()], extra: Annotated[Extra, lib.Form()]):
        return [profile, extra]

    @app.post("/mixed")
    def mixed(profile: Annotated[Profile, lib.Form()], note: Annotated[str, lib.Form()] = "x"):
        return [profile, note]

    @app.post("/json")
    def json_body(count: int = lib.Body(5), profile: Profile = lib.Body(Profile(name="p")), label: str = lib.Body(embed=True)):
        return count

    return app


@pytest.mark.parametrize(
    ("path", "data"),
    [
        ("/union", {"kind": "dog", "bark": "w", "tags": ["a", "b"]}),
        ("/union", {"kind": "cat", "meow": "m", "bark": ""}),
        ("/union", {"kind": "x"}),
        ("/optional-union", {}),
        ("/optional-union", {"kind": "cat", "meow": "m"}),
        ("/optional-union", {"other": "1"}),
    ],
)
def test_form_model_unions(path, data):
    def run(lib):
        response = TestClient(form_app(lib)).post(path, data=data)
        return response.status_code, response.json()

    both(run)


def test_form_model_request_body_schemas():
    def run(lib):
        spec = form_app(lib).openapi()
        bodies = {path: item["post"]["requestBody"] for path, item in spec["paths"].items()}
        return bodies, spec["components"]["schemas"]

    both(run)


def websocket_teardown_app(lib, events):
    app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()

    def session():
        try:
            yield "db"
        except Exception as exc:
            events.append(("sync", type(exc).__name__))
            raise
        events.append(("sync", "clean"))

    async def async_session():
        try:
            yield "adb"
        except Exception as exc:
            events.append(("async", type(exc).__name__))
            raise
        events.append(("async", "clean"))

    def broken(db=lib.Depends(session)):
        raise ValueError("broken dependency")

    @app.websocket("/after-accept")
    async def after_accept(websocket: lib.WebSocket, db=lib.Depends(session), adb=lib.Depends(async_session)):
        await websocket.accept()
        raise KeyError("endpoint")

    @app.websocket("/dependency")
    async def dependency(websocket: lib.WebSocket, value=lib.Depends(broken)):
        await websocket.accept()

    @app.websocket("/validation")
    async def validation(websocket: lib.WebSocket, q: int, db=lib.Depends(session)):
        await websocket.accept()

    @app.websocket("/ok")
    async def ok(websocket: lib.WebSocket, db=lib.Depends(session), adb=lib.Depends(async_session)):
        await websocket.accept()
        await websocket.close()

    return app


WEBSOCKET_TEARDOWN_PATHS = ("/after-accept", "/dependency", "/validation", "/ok")


def test_websocket_errors_reach_yield_dependencies():
    expected = {}
    for path in WEBSOCKET_TEARDOWN_PATHS:
        events = []
        client = TestClient(websocket_teardown_app(fastapi, events), raise_server_exceptions=False)
        try:
            with client.websocket_connect(path) as websocket:
                websocket.receive()
        except BaseException:
            pass
        expected[path] = events

    events = []
    live = LiveServer(websocket_teardown_app(fastrapi, events))
    actual = {}
    for path in WEBSOCKET_TEARDOWN_PATHS:
        events.clear()
        key = base64.b64encode(os.urandom(16)).decode()
        with socket.create_connection(("127.0.0.1", live.port), timeout=5) as sock:
            sock.sendall(
                (
                    f"GET {path} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                    f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
                ).encode()
            )
            try:
                sock.recv(4096)
                sock.recv(4096)
            except OSError:
                pass
        deadline = time.time() + 2
        while len(events) < len(expected[path]) and time.time() < deadline:
            time.sleep(0.02)
        actual[path] = list(events)
    assert actual == expected


def validation_order_app(lib, events):
    app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()

    def auth(token: str | None = lib.Header(None)):
        if token != "ok":
            raise lib.HTTPException(401, "nope", headers={"WWW-Authenticate": "Bearer"})
        events.append("auth")
        return token

    async def async_auth(token: str | None = lib.Header(None)):
        return auth(token)

    def paged(page: int = 1):
        events.append("paged")
        return page

    def session():
        try:
            yield
        except Exception as exc:
            events.append(type(exc).__name__)
            raise

    @app.get("/sync")
    def sync_route(q: int, user=lib.Depends(auth)):
        return q

    @app.get("/async-sync")
    async def async_sync(q: int, user=lib.Depends(auth)):
        return q

    @app.get("/async-async")
    async def async_async(q: int, user=lib.Depends(async_auth)):
        return q

    @app.get("/request")
    def with_request(request: lib.Request, q: int, user=lib.Depends(auth)):
        return q

    @app.get("/yield")
    async def with_yield(q: int, db=lib.Depends(session), user=lib.Depends(auth)):
        return q

    @app.get("/combined")
    def combined(q: int, page=lib.Depends(paged)):
        return q

    @app.get("/route-deps", dependencies=[lib.Depends(auth)])
    def route_deps(q: int):
        return q

    @app.get("/plain")
    def plain(q: int):
        return q

    return app


VALIDATION_ORDER_CASES = [
    ("/sync", {}),
    ("/sync", {"token": "ok"}),
    ("/sync?q=1", {"token": "ok"}),
    ("/async-sync", {}),
    ("/async-sync", {"token": "ok"}),
    ("/async-async", {}),
    ("/async-async", {"token": "ok"}),
    ("/request", {}),
    ("/yield", {}),
    ("/yield", {"token": "ok"}),
    ("/combined?page=x", {}),
    ("/combined?page=x&q=y", {}),
    ("/route-deps", {}),
    ("/plain", {}),
]


@pytest.mark.parametrize(("path", "headers"), VALIDATION_ORDER_CASES)
@pytest.mark.parametrize("override", [False, True])
def test_dependencies_run_before_validation_errors(path, headers, override):
    def run(lib):
        events = []
        app = validation_order_app(lib, events)
        if override:
            app.dependency_overrides[print] = print
        response = TestClient(app, raise_server_exceptions=False).get(path, headers=headers)
        return response.status_code, response.json(), response.headers.get("www-authenticate"), events

    both(run)


def request_state_app(lib):
    @contextlib.asynccontextmanager
    async def lifespan(app):
        yield {"counter": 0, "items": []}

    app = lib.FastAPI(lifespan=lifespan) if lib is fastapi else lib.FastrAPI(lifespan=lifespan)

    def mark(request: lib.Request):
        request.state.marked = "dep"
        return "ok"

    async def amark(request: lib.Request):
        request.state.amarked = "adep"

    @app.get("/shared")
    def shared(request: lib.Request, m=lib.Depends(mark), a=lib.Depends(amark)):
        return [getattr(request.state, "marked", None), getattr(request.state, "amarked", None)]

    @app.get("/shared-async")
    async def shared_async(request: lib.Request, m=lib.Depends(mark)):
        return getattr(request.state, "marked", None)

    @app.get("/no-request")
    def no_request(m=lib.Depends(mark)):
        return "ok"

    @app.get("/bump")
    def bump(request: lib.Request):
        request.state.counter += 1
        request.state.items.append(1)
        return [request.state.counter, len(request.state.items)]

    @app.get("/new")
    def new(request: lib.Request):
        request.state.fresh = getattr(request.state, "fresh", 0) + 1
        return request.state.fresh

    return app


def test_request_state_is_shared_with_dependencies_and_copied_from_lifespan():
    paths = ("/shared", "/shared-async", "/no-request", "/bump", "/bump", "/new", "/new")

    def run(lib):
        with TestClient(request_state_app(lib)) as client:
            return [client.get(path).json() for path in paths]

    expected = run(fastapi)
    assert run(fastrapi) == expected
    live = LiveServer(request_state_app(fastrapi))
    assert [live.http("GET", path)[1] for path in paths] == expected


def test_mounted_app_sees_parent_lifespan_state():
    def build(lib):
        @contextlib.asynccontextmanager
        async def lifespan(app):
            yield {"db": "parent"}

        app = new_app(lib, lifespan=lifespan)
        sub = new_app(lib)

        @sub.get("/state")
        def state(request: lib.Request):
            return getattr(request.state, "db", None)

        app.mount("/sub", sub)
        return app

    def run(lib):
        with TestClient(build(lib)) as client:
            return client.get("/sub/state").json()

    assert run(fastrapi) == run(fastapi)
    assert LiveServer(build(fastrapi)).http("GET", "/sub/state") == (200, "parent")


def catching_route_app(lib):
    class CatchingRoute(lib.routing.APIRoute):
        def get_route_handler(self):
            original = super().get_route_handler()

            async def handler(request):
                try:
                    response = await original(request)
                except lib.exceptions.RequestValidationError as exc:
                    body = await request.body()
                    return lib.responses.JSONResponse({"validation": exc.errors(), "body": body.decode()}, status_code=418)
                except lib.HTTPException as exc:
                    return lib.responses.JSONResponse({"http": exc.detail}, status_code=exc.status_code + 1)
                response.headers["x-wrapped"] = "yes"
                return response

            return handler

    app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()
    router = lib.APIRouter(route_class=CatchingRoute)

    @router.post("/items")
    def create(count: int = lib.Body(embed=True)):
        return {"count": count}

    @router.get("/missing")
    def missing(q: int):
        return q

    @router.get("/teapot")
    def teapot():
        raise lib.HTTPException(400, "bad")

    @router.get("/boom")
    def boom():
        raise ValueError("boom")

    app.include_router(router)
    return app


CATCHING_ROUTE_REQUESTS = [
    ("POST", "/items", {"count": 3}),
    ("POST", "/items", {"count": "x"}),
    ("GET", "/missing", None),
    ("GET", "/missing?q=2", None),
    ("GET", "/teapot", None),
    ("GET", "/boom", None),
]


@pytest.mark.parametrize(("method", "path", "body"), CATCHING_ROUTE_REQUESTS)
def test_route_class_handler_sees_raised_errors(method, path, body):
    def run(lib):
        response = TestClient(catching_route_app(lib), raise_server_exceptions=False).request(method, path, json=body)
        try:
            content = response.json()
        except ValueError:
            content = response.text
        return response.status_code, content, response.headers.get("x-wrapped")

    both(run)


def awaited_security_app(lib):
    security = lib.security

    class Upper(security.HTTPBearer):
        async def __call__(self, request: lib.Request):
            credentials = await super().__call__(request)
            return credentials.credentials.upper() if credentials else None

    class Keyed(security.APIKeyHeader):
        async def __call__(self, request: lib.Request):
            return f"key:{await super().__call__(request)}"

    class Password(security.OAuth2PasswordBearer):
        async def __call__(self, request: lib.Request):
            return f"token:{await super().__call__(request)}"

    optional = security.HTTPBearer(auto_error=False)
    app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()

    @app.get("/upper")
    def upper(token=lib.Depends(Upper())):
        return token

    @app.get("/keyed")
    async def keyed(key=lib.Security(Keyed(name="x-key"))):
        return key

    @app.get("/password")
    def password(token=lib.Depends(Password(tokenUrl="/t"))):
        return token

    @app.get("/plain")
    def plain(credentials=lib.Depends(security.HTTPBasic())):
        return [credentials.username, credentials.password]

    @app.get("/direct")
    async def direct(request: lib.Request):
        credentials = await optional(request)
        return credentials.credentials if credentials else None

    return app


AWAITED_SECURITY_REQUESTS = [
    ("/upper", {"Authorization": "Bearer abc"}),
    ("/upper", {}),
    ("/keyed", {"x-key": "k1"}),
    ("/keyed", {}),
    ("/password", {"Authorization": "Bearer pw"}),
    ("/plain", {"Authorization": "Basic dXNlcjpwYXNz"}),
    ("/plain", {}),
    ("/direct", {"Authorization": "Bearer d"}),
    ("/direct", {}),
]


@pytest.mark.parametrize(("path", "headers"), AWAITED_SECURITY_REQUESTS)
def test_security_schemes_are_awaitable(path, headers):
    def run(lib):
        response = TestClient(awaited_security_app(lib), raise_server_exceptions=False).get(path, headers=headers)
        return response.status_code, response.json(), response.headers.get("www-authenticate")

    both(run)


def test_awaitable_security_subclasses_keep_openapi_schemes():
    def run(lib):
        spec = awaited_security_app(lib).openapi()
        return spec["components"]["securitySchemes"], {path: item["get"].get("security") for path, item in spec["paths"].items()}

    both(run)


def host_app(lib):
    app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()
    api = lib.FastAPI() if lib is fastapi else lib.FastrAPI()

    def real():
        return "real"

    def fake():
        return "fake"

    @api.middleware("http")
    async def tag(request, call_next):
        response = await call_next(request)
        response.headers["x-host-app"] = "api"
        return response

    @api.exception_handler(ValueError)
    async def value_error(request, exc):
        return lib.responses.JSONResponse({"handled": str(exc)}, status_code=409)

    @api.get("/info")
    def info(value=lib.Depends(real)):
        return {"value": value}

    @api.get("/boom")
    def boom():
        raise ValueError("api boom")

    @api.get("/shared")
    def api_shared():
        return "api"

    @app.get("/shared")
    def shared():
        return "parent"

    api.dependency_overrides[real] = fake
    app.host("api.example.com", api)
    return app


HOST_APP_REQUESTS = [
    ("/info", "api.example.com"),
    ("/info", "api.example.com:8000"),
    ("/boom", "api.example.com"),
    ("/shared", "api.example.com"),
    ("/missing", "api.example.com"),
    ("/info", "other.example.com"),
    ("/shared", "other.example.com"),
]


def raw_get(port, path, host):
    with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
        sock.sendall(f"GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n".encode())
        raw = b""
        while chunk := sock.recv(65536):
            raw += chunk
    head, _, body = raw.partition(b"\r\n\r\n")
    lines = head.decode().split("\r\n")
    headers = {name.lower(): value.strip() for name, _, value in (line.partition(":") for line in lines[1:])}
    return int(lines[0].split()[1]), json.loads(body), headers.get("x-host-app")


def test_host_apps_are_isolated():
    def run(lib):
        client = TestClient(host_app(lib), raise_server_exceptions=False)
        responses = [client.get(path, headers={"host": host}) for path, host in HOST_APP_REQUESTS]
        return [(r.status_code, r.json(), r.headers.get("x-host-app")) for r in responses]

    expected = run(fastapi)
    assert run(fastrapi) == expected
    live = LiveServer(host_app(fastrapi))
    assert [raw_get(live.port, path, host) for path, host in HOST_APP_REQUESTS] == expected


def test_background_tasks_subclass_starlette():
    import starlette.background

    def run(lib):
        app = new_app(lib)
        events = []

        @app.get("/tasks")
        def tasks_route(tasks: lib.BackgroundTasks):
            tasks.add_task(events.append, "ran")
            return [
                isinstance(tasks, starlette.background.BackgroundTasks),
                isinstance(tasks, starlette.background.BackgroundTask),
                isinstance(tasks, lib.BackgroundTasks),
            ]

        prefilled = lib.BackgroundTasks(tasks=[starlette.background.BackgroundTask(events.append, "pre")])
        body = TestClient(app).get("/tasks").json()
        time.sleep(0.1)
        return (
            body,
            issubclass(lib.BackgroundTasks, starlette.background.BackgroundTasks),
            [task.args for task in prefilled.tasks],
            events,
        )

    both(run)


def test_api_router_subclasses_starlette_router():
    import starlette.routing

    def run(lib):
        app = new_app(lib)
        router = lib.APIRouter(prefix="/items")
        return (
            isinstance(router, starlette.routing.Router),
            isinstance(app.router, starlette.routing.Router),
            issubclass(lib.routing.APIRouter, starlette.routing.Router),
            lib.APIRouter is lib.routing.APIRouter,
        )

    both(run)


def test_upload_file_subclasses_starlette_upload_file():
    import io

    import starlette.datastructures

    def run(lib):
        app = new_app(lib)

        @app.post("/upload")
        async def upload(file: lib.UploadFile, files: list[lib.UploadFile]):
            return [
                isinstance(file, starlette.datastructures.UploadFile),
                file.filename,
                (await file.read()).decode(),
                [isinstance(item, starlette.datastructures.UploadFile) for item in files],
            ]

        built = lib.UploadFile(io.BytesIO(b"hi"), filename="a.txt")
        response = TestClient(app).post(
            "/upload",
            files=[("file", ("a.txt", b"one")), ("files", ("b.txt", b"two")), ("files", ("c.txt", b"three"))],
        )
        return (
            response.json(),
            isinstance(built, starlette.datastructures.UploadFile),
            issubclass(lib.UploadFile, starlette.datastructures.UploadFile),
            lib.UploadFile is lib.datastructures.UploadFile,
        )

    both(run)


def host_pattern_app(lib):
    app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()
    tenant = lib.FastAPI() if lib is fastapi else lib.FastrAPI()
    numbered = lib.FastAPI() if lib is fastapi else lib.FastrAPI()

    @tenant.get("/who")
    def who(request: lib.Request):
        return {"params": dict(request.path_params), "app": "tenant"}

    @tenant.get("/items/{item}")
    def item(item: str, request: lib.Request):
        return dict(request.path_params)

    @numbered.get("/who")
    def number(request: lib.Request):
        return {"params": dict(request.path_params), "types": [type(v).__name__ for v in request.path_params.values()]}

    @app.get("/parent")
    def parent_missing():
        return "parent"

    app.host("{id:int}.nums.com", numbered)
    app.host("{tenant}.example.com", tenant)
    return app


HOST_PATTERN_REQUESTS = [
    ("/who", "acme.example.com"),
    ("/items/7", "Acme.Example.com:8443"),
    ("/who", "42.nums.com"),
    ("/who", "x.nums.com"),
    ("/missing", "acme.example.com"),
    ("/missing", "other.org"),
]


@pytest.mark.parametrize(("path", "host"), HOST_PATTERN_REQUESTS)
def test_host_patterns(path, host):
    def run(lib):
        response = TestClient(host_pattern_app(lib)).get(path, headers={"host": host})
        return response.status_code, response.json()

    both(run)


class Login(BaseModel):
    user_name: str = Field(validation_alias=AliasChoices("userName", "user"))
    secret: str = Field(validation_alias="pw")
    note: str = Field("x", alias="memo")
    plain: int = 1


class AliasExtra(BaseModel):
    tag: str


def form_alias_app(lib):
    app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()

    @app.post("/single")
    def single(data: Annotated[Login, lib.Form()]):
        return data

    @app.post("/optional")
    def optional(data: Annotated[Login | None, lib.Form()] = None):
        return data

    @app.post("/combined")
    def combined(data: Annotated[Login, lib.Form()], extra: Annotated[AliasExtra, lib.Form()]):
        return data

    return app


FORM_ALIAS_CASES = [
    ("/single", {"userName": "a", "pw": "p"}),
    ("/single", {"user": "b", "pw": "p", "memo": "m"}),
    ("/single", {"user_name": "c", "pw": "p"}),
    ("/optional", {"user": "d", "pw": "p"}),
    ("/optional", {}),
    ("/combined", {"user": "e", "pw": "p", "tag": "t"}),
]


def test_form_model_alias_schemas():
    both(lambda lib: form_alias_app(lib).openapi())


@pytest.mark.parametrize(("path", "data"), FORM_ALIAS_CASES)
def test_form_model_alias_requests(path, data):
    def run(lib):
        response = TestClient(form_alias_app(lib)).post(path, data=data)
        return response.status_code, response.json()

    both(run)
