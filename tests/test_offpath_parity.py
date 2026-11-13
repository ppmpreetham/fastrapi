import fastapi
import fastapi.responses
import pytest
from fastapi.testclient import TestClient

import fastrapi
import fastrapi.responses


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def submodule(lib, name):
    return __import__(f"{lib.__name__}.{name}", fromlist=["_"])


def client(app):
    return TestClient(app, raise_server_exceptions=False, follow_redirects=False)


def both(run):
    assert run(fastrapi) == run(fastapi)


def test_json_response_subclasses_match_fastapi():
    def run(lib):
        r = lib.responses
        return [issubclass(cls, r.JSONResponse) for cls in (r.ORJSONResponse, r.UJSONResponse)]

    both(run)


@pytest.mark.filterwarnings("ignore::DeprecationWarning")
def test_json_response_subclasses_render():
    r = fastrapi.responses
    app = fastrapi.FastrAPI()

    @app.get("/o")
    def o():
        return r.ORJSONResponse({"a": 1}, status_code=201, headers={"x-a": "1"})

    @app.get("/u")
    def u():
        return r.UJSONResponse({"b": 2})

    c = client(app)
    assert [(x.status_code, x.json(), x.headers.get("x-a")) for x in (c.get("/o"), c.get("/u"))] == [
        (201, {"a": 1}, "1"),
        (200, {"b": 2}, None),
    ]


def test_event_source_response_hierarchy_matches_fastapi():
    def run(lib):
        r = lib.responses
        app = new_app(lib)

        @app.get("/sse", response_class=r.EventSourceResponse)
        def sse():
            yield {"a": 1}

        x = client(app).get("/sse")
        return issubclass(r.EventSourceResponse, r.StreamingResponse), x.headers["content-type"], x.text

    both(run)


def test_strict_password_form_hierarchy_matches_fastapi():
    def run(lib):
        s = submodule(lib, "security")
        form = s.OAuth2PasswordRequestFormStrict(grant_type="password", username="u", password="p", scope="a b")
        return (
            issubclass(s.OAuth2PasswordRequestFormStrict, s.OAuth2PasswordRequestForm),
            (form.grant_type, form.username, form.password, form.scopes, form.client_id),
        )

    both(run)


def test_websocket_exception_matches_fastapi():
    import starlette.exceptions

    def run(lib):
        exc = lib.WebSocketException(code=1008, reason="no")
        return (
            issubclass(lib.WebSocketException, starlette.exceptions.WebSocketException),
            lib.WebSocketException is submodule(lib, "exceptions").WebSocketException,
            str(exc),
            repr(exc),
        )

    both(run)


def test_user_middleware_matches_fastapi():
    def run(lib):
        gzip = submodule(lib, "middleware.gzip").GZipMiddleware
        cors = submodule(lib, "middleware.cors").CORSMiddleware
        app = new_app(lib, middleware=[submodule(lib, "middleware").Middleware(gzip, minimum_size=5)])
        app.add_middleware(cors, allow_origins=["*"])

        @app.middleware("http")
        async def mw(request, call_next):
            return await call_next(request)

        return [
            (entry.cls.__name__, entry.args, entry.kwargs.get("allow_origins"), entry.kwargs.get("minimum_size"))
            for entry in app.user_middleware
        ]

    both(run)


def test_api_route_attributes_match_fastapi():
    from pydantic import BaseModel

    class Out(BaseModel):
        a: int
        b: int | None = None

    def run(lib):
        app = new_app(lib)

        @app.get("/items/{item_id:int}", response_model=Out, response_model_exclude_none=True)
        def item(item_id: int):
            return {"a": item_id}

        @app.get("/html", response_class=lib.responses.HTMLResponse, operation_id="html_op")
        def html():
            return "<p/>"

        def attrs(route):
            return (
                route.unique_id,
                route.path_format,
                route.path_regex.pattern,
                sorted(route.param_convertors),
                getattr(route.response_class, "value", route.response_class).__name__,
                route.response_model_include,
                route.response_model_exclude,
                route.response_model_by_alias,
                route.response_model_exclude_unset,
                route.response_model_exclude_none,
                route.dependency_overrides_provider is not None,
            )

        manual = submodule(lib, "routing").APIRoute("/m/{x}", item, methods=["post"])
        routes = sorted((attrs(r) for r in app.routes if getattr(r, "path", "") in ("/items/{item_id:int}", "/html")))
        return routes, attrs(manual)

    both(run)


def test_static_files_methods_match_fastapi(tmp_path):
    import asyncio

    import httpx

    (tmp_path / "a.txt").write_text("hello")

    def run(lib):
        files = submodule(lib, "staticfiles").StaticFiles(directory=str(tmp_path))

        async def call():
            transport = httpx.ASGITransport(app=files)
            async with httpx.AsyncClient(transport=transport, base_url="http://t") as c:
                r = await c.get("/a.txt")
                return r.status_code, r.text

        return (
            files.get_directories(),
            files.get_directories(str(tmp_path)) == [str(tmp_path)],
            asyncio.run(call()),
        )

    both(run)


def test_mount_names_match_fastapi(tmp_path):
    def run(lib):
        sub = new_app(lib)

        @sub.get("/items/{item_id}")
        def item(item_id: int):
            return {}

        hosted = new_app(lib)

        @hosted.get("/h")
        def h():
            return {}

        app = new_app(lib)
        app.mount("/static", submodule(lib, "staticfiles").StaticFiles(directory=str(tmp_path)), name="static")
        app.mount("/sub", sub, name="api")
        app.host("api.example.com", hosted, name="hosted")
        urls = [
            app.url_path_for("static", path="/css/a.css"),
            app.url_path_for("api:item", item_id=3),
            app.url_path_for("hosted:h"),
        ]
        return [(str(u), u.protocol, u.host) for u in urls]

    both(run)


def test_non_ascii_paths_match_fastapi():
    def run(lib):
        app = new_app(lib)

        @app.get("/ünï")
        def unicode():
            return {"ok": 1}

        @app.get("/café/{item}")
        def cafe(item: str):
            return {"item": item}

        c = client(app)
        return [(r.status_code, r.json()) for r in (c.get("/ünï"), c.get("/%C3%BCn%C3%AF"), c.get("/café/x"))]

    both(run)


def test_prefixed_empty_and_slash_routes_match_fastapi():
    def run(lib):
        app = new_app(lib)
        router = lib.APIRouter(prefix="/items")

        @router.get("")
        def empty():
            return "empty"

        @router.get("/")
        def slash():
            return "slash"

        app.include_router(router)
        c = client(app)
        return [(r.status_code, r.json()) for r in (c.get("/items"), c.get("/items/"))]

    both(run)


def test_error_after_yield_keeps_response_like_fastapi():
    def run(lib):
        app = new_app(lib)

        def dep():
            yield 1
            raise RuntimeError("after")

        async def adep():
            yield 1
            raise RuntimeError("after")

        @app.get("/s")
        def s(d=lib.Depends(dep)):
            return "ok"

        @app.get("/a")
        async def a(d=lib.Depends(adep)):
            return "ok"

        responses = [(r.status_code, r.json()) for r in (client(app).get("/s"), client(app).get("/a"))]
        for path in ("/s", "/a"):
            with pytest.raises(RuntimeError, match="after"):
                TestClient(app).get(path)
        return responses

    both(run)


def test_static_errors_use_handlers_like_fastapi(tmp_path):
    import starlette.exceptions

    (tmp_path / "a.txt").write_text("a")

    def run(lib):
        app = new_app(lib)
        app.mount("/st", submodule(lib, "staticfiles").StaticFiles(directory=str(tmp_path)))

        @app.exception_handler(404)
        def not_found(request, exc):
            return lib.responses.JSONResponse({"custom": exc.status_code, "path": request.url.path}, status_code=404)

        @app.exception_handler(starlette.exceptions.HTTPException)
        def http_error(request, exc):
            return lib.responses.JSONResponse({"http": exc.status_code}, status_code=exc.status_code)

        c = client(app)
        return [(r.status_code, r.json()) for r in (c.get("/st/missing.txt"), c.post("/st/a.txt"))]

    both(run)


def test_dependency_cache_is_shared_like_fastapi():
    def run(lib):
        calls = []

        def get_db():
            calls.append("db")
            return 1

        async def get_async():
            calls.append("async")
            return 2

        def sub(db=lib.Depends(get_db)):
            calls.append("sub")
            return db

        app = new_app(lib, dependencies=[lib.Depends(get_db)])
        router = lib.APIRouter(dependencies=[lib.Depends(get_async)])

        @router.get("/mixed", dependencies=[lib.Depends(sub)])
        def mixed(s=lib.Depends(sub), db=lib.Depends(get_db), a=lib.Depends(get_async)):
            return [s, db, a]

        @app.get("/twice")
        async def twice(a=lib.Depends(get_db), b=lib.Depends(get_db)):
            return [a, b]

        app.include_router(router)
        c = client(app)
        results = []
        for path in ("/mixed", "/twice"):
            calls.clear()
            results.append((c.get(path).json(), sorted(calls)))
        return results

    both(run)


def test_security_classes_match_fastapi():
    def run(lib):
        s = submodule(lib, "security")
        base = submodule(lib, "security.api_key")

        class Forbidden(s.HTTPBearer):
            def make_not_authenticated_error(self):
                return lib.HTTPException(status_code=403, detail="nope")

        class Token(base.APIKeyBase):
            def __init__(self):
                super().__init__(location="header", name="x-token", description="t", scheme_name=None, auto_error=True)

            async def __call__(self, request: lib.Request):
                return self.check_api_key(request.headers.get("x-token"))

        bearer = Forbidden()
        token = Token()
        app = new_app(lib)

        @app.get("/bearer")
        def protected(credentials=lib.Depends(bearer)):
            return credentials.credentials

        @app.get("/token")
        def tokened(value=lib.Depends(token)):
            return value

        c = client(app)
        basic = s.HTTPBasic(realm="r")
        password = s.OAuth2PasswordBearer(tokenUrl="t", scopes={"a": "A"})
        key = s.APIKeyQuery(name="k")
        return (
            [(r.status_code, r.json()) for r in (c.get("/bearer"), c.get("/token"), c.get("/token", headers={"x-token": "v"}))],
            bearer.scheme_name,
            token.scheme_name,
            basic.make_authenticate_headers(),
            (basic.make_not_authenticated_error().status_code, basic.make_not_authenticated_error().headers),
            key.check_api_key("v"),
            key.make_not_authenticated_error().headers,
            password.make_not_authenticated_error().headers,
            [m.model.model_dump(mode="json", by_alias=True, exclude_none=True) for m in (basic, password, key, s.HTTPBearer(bearerFormat="JWT"))],
            sorted(app.openapi()["components"]["securitySchemes"]),
        )

    both(run)


def test_websocket_accept_options_match_fastapi():
    import base64
    import os
    import socket

    from test_websocket_smoke import _free_port, _serve

    def handler(lib):
        async def ws(websocket: lib.WebSocket):
            await websocket.accept(subprotocol="chat", headers=[(b"x-extra", b"1")])
            await websocket.close()

        return ws

    expected_app = fastapi.FastAPI()
    expected_app.websocket("/ws")(handler(fastapi))
    with TestClient(expected_app).websocket_connect("/ws", subprotocols=["chat"]) as websocket:
        expected = (websocket.accepted_subprotocol, dict(websocket.extra_headers).get(b"x-extra"))

    app = fastrapi.FastrAPI()
    app.websocket("/ws")(handler(fastrapi))
    port = _free_port()
    _serve(app, port)
    key = base64.b64encode(os.urandom(16)).decode()
    with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
        sock.sendall(
            (
                "GET /ws HTTP/1.1\r\n"
                f"Host: 127.0.0.1:{port}\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
                "Sec-WebSocket-Protocol: chat\r\n\r\n"
            ).encode()
        )
        head = sock.recv(4096).split(b"\r\n\r\n")[0].decode().lower().split("\r\n")
    headers = dict(line.split(": ", 1) for line in head[1:])
    assert head[0].startswith("http/1.1 101")
    assert (headers.get("sec-websocket-protocol"), headers.get("x-extra", "").encode()) == expected


def test_middleware_classes_as_asgi_apps_match_fastapi():
    import asyncio

    import httpx

    async def inner(scope, receive, send):
        body = b"x" * 600
        await send({"type": "http.response.start", "status": 200, "headers": [(b"content-type", b"text/plain")]})
        await send({"type": "http.response.body", "body": body})

    def run(lib):
        m = lambda name: submodule(lib, f"middleware.{name}")
        apps = [
            m("cors").CORSMiddleware(inner, allow_origins=["https://a.com"]),
            m("gzip").GZipMiddleware(inner, minimum_size=10),
            m("trustedhost").TrustedHostMiddleware(inner, allowed_hosts=["t"]),
            m("httpsredirect").HTTPSRedirectMiddleware(inner),
        ]

        async def call(app):
            transport = httpx.ASGITransport(app=app)
            async with httpx.AsyncClient(transport=transport, base_url="http://t") as c:
                r = await c.get("/p", headers={"origin": "https://a.com", "accept-encoding": "gzip"})
                return r.status_code, r.headers.get("access-control-allow-origin"), r.headers.get("content-encoding"), r.headers.get("location")

        return [asyncio.run(call(app)) for app in apps], [app.app is inner for app in apps]

    both(run)


def test_background_tasks_call_matches_fastapi():
    import asyncio

    def run(lib):
        calls = []

        async def later(value):
            calls.append(("async", value))

        tasks = lib.BackgroundTasks()
        tasks.add_task(calls.append, "sync")
        tasks.add_task(later, 1)
        tasks.add_task(calls.append, "last")
        asyncio.run(tasks())
        return calls, len(tasks.tasks)

    both(run)


def test_responses_as_asgi_apps_match_fastapi():
    import asyncio

    import httpx

    def run(lib):
        r = lib.responses
        calls = []
        tasks = lib.BackgroundTasks()
        tasks.add_task(calls.append, "done")
        responses = [
            r.JSONResponse({"a": 1}, status_code=201, headers={"x-a": "1"}, background=tasks),
            r.PlainTextResponse("hi"),
            r.HTMLResponse("<p/>"),
            r.RedirectResponse("/to"),
            r.Response(b"raw", media_type="application/octet-stream"),
        ]

        async def call(response):
            transport = httpx.ASGITransport(app=response)
            async with httpx.AsyncClient(transport=transport, base_url="http://t") as c:
                got = await c.get("/")
                return got.status_code, got.content, got.headers.get("content-type"), got.headers.get("x-a"), got.headers.get("location")

        return [asyncio.run(call(response)) for response in responses], calls

    both(run)


def test_request_hierarchy_matches_fastapi():
    import collections.abc

    import starlette.requests
    import starlette.websockets

    def run(lib):
        app = new_app(lib)

        @app.get("/r")
        def r(request: lib.Request):
            return [
                isinstance(request, starlette.requests.Request),
                isinstance(request, starlette.requests.HTTPConnection),
                isinstance(request, collections.abc.Mapping),
                request["type"],
            ]

        return client(app).get("/r").json(), issubclass(lib.WebSocket, starlette.websockets.WebSocket)

    both(run)


def test_form_data_hierarchy_matches_fastapi():
    import collections.abc

    import starlette.datastructures as datastructures

    def run(lib):
        app = new_app(lib)

        @app.post("/f")
        async def f(request: lib.Request):
            form = await request.form()
            return [
                isinstance(form, datastructures.ImmutableMultiDict),
                isinstance(form, datastructures.FormData),
                isinstance(form, collections.abc.Mapping),
                form.getlist("a"),
                list(form.multi_items()),
            ]

        return client(app).post("/f", data={"a": ["1", "2"], "b": "3"}).json()

    both(run)


def session_app(lib):
    from starlette.middleware.sessions import SessionMiddleware

    app = new_app(lib)
    app.add_middleware(SessionMiddleware, secret_key="secret", max_age=60)

    @app.get("/count")
    def count(request: lib.Request):
        request.session["n"] = request.session.get("n", 0) + 1
        return dict(request.session)

    @app.get("/read")
    def read(request: lib.Request):
        return dict(request.session)

    @app.get("/clear")
    def clear(request: lib.Request):
        request.session.clear()
        return {}

    @app.get("/none")
    def untouched():
        return {}

    return app


def test_sessions_match_fastapi():
    def run(lib):
        c = client(session_app(lib))
        first = c.get("/count")
        second = c.get("/count")
        untouched = c.get("/none")
        read = c.get("/read")
        cleared = c.get("/clear")
        cookie = first.headers["set-cookie"]
        return (
            first.json(),
            second.json(),
            read.json(),
            read.headers.get("vary"),
            "set-cookie" in untouched.headers,
            cookie.split("=", 1)[0],
            cookie.split(";", 1)[1],
            cleared.headers.get("set-cookie"),
            c.get("/read", cookies={"session": "tampered"}).json(),
        )

    both(run)


def test_session_cookies_interoperate_with_fastapi():
    ours, theirs = client(session_app(fastrapi)), client(session_app(fastapi))
    ours.get("/count")
    theirs.cookies = ours.cookies
    assert theirs.get("/count").json() == {"n": 2}
    ours.cookies = theirs.cookies
    assert ours.get("/count").json() == {"n": 3}


def test_route_callbacks_match_fastapi():
    def run(lib):
        callbacks = lib.APIRouter()

        @callbacks.post("{$callback_url}/events")
        def event():
            pass

        router = lib.APIRouter(callbacks=callbacks.routes)
        app = new_app(lib)

        @app.post("/direct", callbacks=callbacks.routes)
        def direct():
            pass

        @app.get("/plain")
        def plain():
            pass

        @router.post("/inherited")
        def inherited():
            pass

        app.include_router(router)
        found = {r.path: [c.path for c in r.callbacks] for r in app.routes if getattr(r, "path", "") in ("/direct", "/plain")}
        return found, [c.path for c in router.routes[0].callbacks]

    both(run)


def test_form_options_match_fastapi():
    def run(lib):
        app = new_app(lib)

        @app.post("/limited")
        async def limited(request: lib.Request):
            form = await request.form(max_fields=1, max_files=1, max_part_size=4)
            return sorted(form.keys())

        @app.post("/managed")
        async def managed(request: lib.Request):
            async with request.form() as form:
                return {"a": form["a"]}

        c = client(app)
        cases = [
            c.post("/limited", data={"a": "1"}),
            c.post("/limited", data={"a": "1", "b": "2"}),
            c.post("/limited", data={"a": "12345"}),
            c.post("/limited", files=[("f", ("a.txt", b"x")), ("g", ("b.txt", b"y"))]),
            c.post("/limited", data={"a": "1"}, files=[("f", ("a.txt", b"x"))]),
            c.post("/limited", data={"a": "123456"}, files=[("f", ("a.txt", b"x"))]),
            c.post("/managed", data={"a": "1"}),
        ]
        return [(r.status_code, r.json()) for r in cases]

    both(run)




def json_mode_handler(lib):
    async def ws(websocket: lib.WebSocket):
        await websocket.accept()
        data = await websocket.receive_json(mode="binary")
        await websocket.send_json(data, mode="binary")
        data = await websocket.receive_json()
        await websocket.send_json(data)
        try:
            await websocket.send_json(data, mode="bad")
        except RuntimeError as error:
            await websocket.send_text(str(error))
        await websocket.close()

    return ws


def test_websocket_json_modes_match_fastapi():
    import base64
    import os
    import socket
    import struct

    from test_websocket_smoke import _free_port, _recv_frame, _send_text, _serve

    expected_app = fastapi.FastAPI()
    expected_app.websocket("/ws")(json_mode_handler(fastapi))
    with TestClient(expected_app).websocket_connect("/ws") as websocket:
        websocket.send_bytes(b'{"a":1}')
        binary = websocket.receive_bytes()
        websocket.send_text('{"b":2}')
        expected = [binary, websocket.receive_text(), websocket.receive_text()]

    app = fastrapi.FastrAPI()
    app.websocket("/ws")(json_mode_handler(fastrapi))
    port = _free_port()
    _serve(app, port)
    key = base64.b64encode(os.urandom(16)).decode()
    with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
        sock.sendall(
            (
                f"GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        assert b" 101 " in sock.recv(4096)
        payload, mask = b'{"a":1}', os.urandom(4)
        sock.sendall(bytes([0x82, 0x80 | len(payload)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))
        frames = [_recv_frame(sock)]
        _send_text(sock, '{"b":2}')
        frames += [_recv_frame(sock), _recv_frame(sock)]
    assert [opcode for opcode, _ in frames] == [0x2, 0x1, 0x1]
    assert [frames[0][1], frames[1][1].decode(), frames[2][1].decode()] == expected


def test_request_scope_matches_fastapi():
    keys = ("type", "method", "path", "raw_path", "query_string", "root_path", "scheme", "http_version", "client", "path_params")

    def run(lib):
        app = new_app(lib)

        @app.get("/s/{x}")
        def s(x: str, request: lib.Request):
            request.headers, request.cookies, request.query_params
            scope = request.scope
            scope["headers"].append((b"x-added", b"1"))
            return {
                "scope": {key: repr(scope.get(key)) for key in keys},
                "server": tuple(scope["server"]),
                "private": sorted(key for key in scope if key.startswith("_")),
                "headers": isinstance(scope["headers"], list),
                "url": [str(request.url), request.url.path, request.path_params, x],
            }

        return client(app).get("/s/a%20b?q=1", cookies={"c": "1"}).json()

    both(run)


def test_server_sent_event_model_matches_fastapi():
    def run(lib):
        event_cls = submodule(lib, "sse").ServerSentEvent
        event = event_cls(data={"a": 1}, event="x")
        validated = event_cls.model_validate({"data": 1, "id": "i"})
        return (
            list(event_cls.model_fields),
            event_cls.model_json_schema(),
            event.model_dump(),
            event.model_dump(exclude_none=True),
            event.model_dump(exclude_unset=True),
            event.model_dump(include={"event"}),
            event.model_dump_json(),
            sorted(event.model_fields_set),
            repr(validated),
            str(validated),
            event.model_copy(update={"id": "2"}).model_dump(exclude_none=True),
            event == event_cls(data={"a": 1}, event="x"),
        )

    both(run)


def test_middleware_request_url_matches_fastapi():
    def run(lib):
        app = new_app(lib)
        seen = {}

        @app.middleware("http")
        async def record(request, call_next):
            seen["url"] = str(request.url)
            seen["base"] = str(request.base_url)
            seen["host"] = request.headers.get("host")
            seen["path"] = request.url.path
            return await call_next(request)

        @app.get("/items/{x}")
        def items(x: str):
            return x

        client(app).get("/items/a%20b?q=1")
        return seen

    both(run)


def test_mount_any_asgi_app_matches_fastapi(tmp_path):
    import json

    import starlette.applications
    import starlette.responses
    import starlette.routing
    import starlette.staticfiles

    (tmp_path / "a.txt").write_text("static")

    async def raw(scope, receive, send):
        body = json.dumps({"path": scope["path"], "root_path": scope["root_path"]}).encode()
        await send({"type": "http.response.start", "status": 200, "headers": [(b"content-type", b"application/json")]})
        await send({"type": "http.response.body", "body": body})

    async def hello(request):
        return starlette.responses.PlainTextResponse(f"hello {request.url.path}")

    def run(lib):
        sub = starlette.applications.Starlette(routes=[starlette.routing.Route("/hi", hello)])
        app = new_app(lib)
        app.mount("/raw", raw, name="raw")
        app.mount("/star", sub)
        app.mount("/files", starlette.staticfiles.StaticFiles(directory=str(tmp_path)))

        @app.get("/own")
        def own():
            return "own"

        c = client(app)
        responses = [c.get(path) for path in ("/raw/x/y", "/star/hi", "/files/a.txt", "/own", "/files/missing")]
        return [(r.status_code, r.text) for r in responses], str(app.url_path_for("raw", path="/z"))

    both(run)


def test_mounted_inside_asgi_app_matches_fastapi():
    def run(lib):
        inner = new_app(lib)

        @inner.get("/x/{v}")
        def x(v: str, request: lib.Request):
            scope = request.scope
            return [
                str(request.url),
                request.url.path,
                scope["root_path"],
                scope["path"],
                str(request.base_url),
                str(request.url_for("x", v="q")),
            ]

        outer = fastapi.FastAPI()
        outer.mount("/sub", inner)
        return client(outer).get("/sub/x/a").json()

    both(run)



def test_router_attributes_match_fastapi():
    import asyncio

    def run(lib):
        router = lib.APIRouter()
        sent = []

        async def receive():
            return {"type": "http.disconnect"}

        async def send(message):
            sent.append({k: v for k, v in message.items() if k != "headers"})

        asyncio.run(router.not_found({"type": "http", "path": "/x"}, receive, send))
        match, child = router.matches({"type": "http", "path": "/x", "method": "GET"})
        return sent, match.name, child, [callable(getattr(router, name)) for name in ("app", "handle", "lifespan")]

    both(run)


def test_router_is_an_asgi_app():
    import asyncio

    import httpx

    router = fastrapi.APIRouter(prefix="/r")

    @router.get("/items/{item_id}")
    def item(item_id: int):
        return {"item_id": item_id}

    async def call():
        transport = httpx.ASGITransport(app=router)
        async with httpx.AsyncClient(transport=transport, base_url="http://t") as c:
            return [(r.status_code, r.json()) for r in [await c.get(p) for p in ("/r/items/3", "/r/missing", "/docs")]]

    assert asyncio.run(call()) == [(200, {"item_id": 3}), (404, {"detail": "Not Found"}), (404, {"detail": "Not Found"})]



def test_request_receive_matches_fastapi():
    def run(lib):
        app = new_app(lib)

        @app.post("/copy")
        async def copy(request: lib.Request):
            clone = lib.Request(request.scope, request.receive)
            return {"clone": (await clone.body()).decode(), "callable": callable(request.receive)}

        return client(app).post("/copy", content=b"abc").json()

    both(run)


def test_custom_route_class_matches_fastapi():
    def run(lib):
        routing = submodule(lib, "routing")

        class TimedRoute(routing.APIRoute):
            def get_route_handler(self):
                original = super().get_route_handler()

                async def handler(request: lib.Request):
                    response = await original(request)
                    response.headers["x-route"] = "timed"
                    return response

                return handler

        class UpperRequest(lib.Request):
            async def body(self):
                return (await super().body()).upper()

        class UpperRoute(routing.APIRoute):
            def get_route_handler(self):
                original = super().get_route_handler()

                async def handler(request: lib.Request):
                    return await original(UpperRequest(request.scope, request.receive))

                return handler

        app = new_app(lib)
        timed = lib.APIRouter(route_class=TimedRoute)
        upper = lib.APIRouter(route_class=UpperRoute)

        @timed.get("/items/{item_id}")
        def item(item_id: int, q: str = "x"):
            return {"item_id": item_id, "q": q}

        @upper.post("/echo")
        async def echo(request: lib.Request):
            return {"body": (await request.body()).decode()}

        @app.get("/plain")
        def plain():
            return "plain"

        app.include_router(timed)
        app.include_router(upper)
        c = client(app)
        responses = [c.get("/items/3?q=y"), c.post("/echo", content=b"abc"), c.get("/plain")]
        return [(r.status_code, r.json(), r.headers.get("x-route")) for r in responses]

    both(run)
