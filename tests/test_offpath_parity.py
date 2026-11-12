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
