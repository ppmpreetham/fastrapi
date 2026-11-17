import inspect
from collections.abc import AsyncIterable
from typing import Annotated

import fastapi
import fastapi.utils
import pytest
from fastapi.testclient import TestClient

import fastrapi
from conftest import LiveServerTestClient
from fastrapi import utils as fastrapi_utils


def app_for(lib):
    return lib.FastAPI() if lib is fastapi else lib.FastrAPI()


def endpoint():
    return {}


def both(build):
    return build(fastapi), build(fastrapi)


def test_route_argument_names():
    for lib in (fastapi, fastrapi):

        async def handler(request):
            return None

        app_for(lib).add_route("/x", route=handler)
        lib.APIRouter().add_route("/x", endpoint=handler)
        lib.APIRouter().add_websocket_route("/w", endpoint=handler)


@pytest.mark.parametrize(
    "build",
    [
        lambda lib: lib.Depends(endpoint),
        lambda lib: lib.Depends(endpoint, use_cache=False),
        lambda lib: lib.Security(endpoint, scopes=["a"]),
        lambda lib: lib.Security(endpoint),
    ],
)
def test_depends_repr(build):
    expected, actual = both(build)
    assert repr(actual) == repr(expected)


def test_depends_equality_and_hash():
    for lib in (fastapi, fastrapi):
        assert lib.Depends(endpoint) == lib.Depends(endpoint)
        assert lib.Depends(endpoint) != lib.Depends(endpoint, use_cache=False)
        assert hash(lib.Depends(endpoint)) == hash(lib.Depends(endpoint))
        assert lib.Security(endpoint, scopes=("a",)) == lib.Security(endpoint, scopes=("a",))
        assert lib.Depends(endpoint) != lib.Security(endpoint)
        with pytest.raises(TypeError):
            hash(lib.Security(endpoint, scopes=["a"]))


def test_deep_dict_update_argument_names():
    assert list(inspect.signature(fastrapi_utils.deep_dict_update).parameters) == list(
        inspect.signature(fastapi.utils.deep_dict_update).parameters
    )


def test_websocket_exception_repr():
    for args in ((1008,), (1008, "pol")):
        expected, actual = both(lambda lib: lib.WebSocketException(*args))
        assert repr(actual) == repr(expected)
        assert actual.reason == expected.reason


def test_route_and_router_defaults():
    def route_defaults(lib):
        app = app_for(lib)
        app.get("/r")(endpoint)
        route = next(r for r in app.routes if getattr(r, "path", None) == "/r")
        return route.callbacks, route.responses, route.description, route.response_description

    assert route_defaults(fastrapi) == route_defaults(fastapi)
    expected, actual = both(lambda lib: lib.APIRouter())
    assert (actual.dependencies, actual.responses, actual.callbacks) == (
        expected.dependencies,
        expected.responses,
        expected.callbacks,
    )
    assert actual.route_class.__name__ == "APIRoute"


def openapi(lib, register):
    app = app_for(lib)
    register(app, lib)
    return app.openapi()


def test_docstring_form_feed():
    def register(app, lib):
        @app.get("/d")
        def documented():
            """Shown.

            \fHidden."""

    expected, actual = both(lambda lib: openapi(lib, register)["paths"]["/d"]["get"].get("description"))
    assert actual == expected


def test_parameter_order():
    def register(app, lib):
        def paging(limit: int = 10, x_tenant: Annotated[str, lib.Header()] = "a"):
            return limit

        @app.get("/p/{a}")
        def params(
            c: Annotated[str, lib.Cookie()],
            h: Annotated[str, lib.Header()],
            q: str,
            a: int,
            page: int = lib.Depends(paging),
        ):
            pass

    expected, actual = both(
        lambda lib: [(p["in"], p["name"]) for p in openapi(lib, register)["paths"]["/p/{a}"]["get"]["parameters"]]
    )
    assert actual == expected


@pytest.mark.parametrize(
    "annotation",
    [
        lambda lib: Annotated[list[str], lib.Query(min_length=1, max_length=3)],
        lambda lib: Annotated[int, lib.Query(title="Item")],
        lambda lib: Annotated[int, lib.Query(description="desc", deprecated=True)],
        lambda lib: Annotated[int, lib.Query(json_schema_extra={"x-extra": 1})],
    ],
)
def test_parameter_schema(annotation):
    def register(app, lib):
        hint = annotation(lib)

        @app.get("/s")
        def param(v: hint):
            pass

    expected, actual = both(lambda lib: openapi(lib, register)["paths"]["/s"]["get"]["parameters"][0])
    assert actual == expected


def test_upload_file_schema():
    def register(app, lib):
        @app.post("/u")
        def upload(f: lib.UploadFile, g: list[lib.UploadFile]):
            pass

    def body(lib):
        schemas = openapi(lib, register)["components"]["schemas"]
        return next(v for k, v in schemas.items() if k.startswith("Body"))["properties"]

    expected, actual = both(body)
    assert actual == expected


def response_app(lib):
    app = app_for(lib)

    @app.get("/delete")
    def delete():
        response = lib.Response()
        response.delete_cookie("k", partitioned=True)
        return response

    @app.get("/stream")
    def stream():
        return lib.responses.StreamingResponse(iter([b"a"]), media_type="text/plain")

    return app


def test_response_headers():
    from fastapi.sse import EventSourceResponse

    def headers(client):
        cookie = client.get("/delete").headers["set-cookie"]
        return cookie.split("; ")[0], "Partitioned" in cookie, client.get("/stream").headers["content-type"]

    with LiveServerTestClient(response_app(fastrapi)) as client:
        actual = headers(client)
    assert actual == headers(TestClient(response_app(fastapi)))

    for lib, cls in ((fastapi, EventSourceResponse), (fastrapi, fastrapi.sse.EventSourceResponse)):
        app = app_for(lib)

        @app.get("/sse", response_class=cls)
        async def events():
            yield {"a": 1}

        if lib is fastapi:
            assert TestClient(app).get("/sse").headers["content-type"] == "text/event-stream; charset=utf-8"
        else:
            with LiveServerTestClient(app) as client:
                assert client.get("/sse").headers["content-type"] == "text/event-stream; charset=utf-8"


def test_route_context_attributes():
    def attributes(lib):
        app = app_for(lib)
        app.get("/ctx/{item}")(endpoint)
        route = next(r for r in app.routes if getattr(r, "path", None) == "/ctx/{item}")
        context = lib.routing.RouteContext(route)
        return context.path, context.path_format, context.name, context.methods, context.endpoint is endpoint

    expected, actual = both(attributes)
    assert actual == expected


def test_router_matches_scope_keyword():
    expected, actual = both(lambda lib: lib.APIRouter().matches(scope={"type": "http"}))
    assert actual == expected


def test_request_form_limit_defaults_are_ints():
    def defaults(lib):
        parameters = inspect.signature(lib.Request.form).parameters
        return {name: parameters[name].default for name in ("max_files", "max_fields", "max_part_size")}

    expected, actual = both(defaults)
    assert actual == expected
    assert all(type(value) is int for value in actual.values())


def test_route_class_assignable_after_creation():
    def header(lib):
        class Wrapped(lib.routing.APIRoute):
            def get_route_handler(self):
                original = super().get_route_handler()

                async def handler(request):
                    response = await original(request)
                    response.headers["x-wrapped"] = "yes"
                    return response

                return handler

        router = lib.APIRouter()
        router.route_class = Wrapped
        router.get("/x")(endpoint)
        app = app_for(lib)
        app.include_router(router)
        if lib is fastapi:
            return TestClient(app).get("/x").headers.get("x-wrapped")
        with LiveServerTestClient(app) as client:
            return client.get("/x").headers.get("x-wrapped")

    expected, actual = both(header)
    assert actual == expected == "yes"


def test_include_router_response_class_inheritance():
    def media_types(lib):
        def level(number):
            return type(f"Level{number}", (lib.responses.JSONResponse,), {"media_type": f"application/x-level-{number}"})

        app = app_for(lib)
        app.default_response_class = level(0)
        outer, inner, leaf = lib.APIRouter(default_response_class=level(1)), lib.APIRouter(), lib.APIRouter()
        leaf.get("/c")(endpoint)
        leaf.get("/d", response_class=level(5))(endpoint)
        inner.include_router(leaf, prefix="/b", default_response_class=level(3))
        outer.include_router(inner, prefix="/a")
        outer.get("/own")(endpoint)
        app.include_router(outer, prefix="/x", default_response_class=level(4))
        app.get("/app")(endpoint)
        paths = ("/app", "/x/own", "/x/a/b/c", "/x/a/b/d")
        if lib is fastapi:
            client = TestClient(app)
            return [client.get(path).headers["content-type"] for path in paths]
        with LiveServerTestClient(app) as client:
            return [client.get(path).headers["content-type"] for path in paths]

    expected, actual = both(media_types)
    assert actual == expected


@pytest.mark.parametrize("kind", ["Path", "Query"])
def test_union_mode_on_path_and_query(kind):
    def run(lib):
        marker = getattr(lib, kind)(union_mode="left_to_right")
        app = app_for(lib)
        path = "/u/{v}" if kind == "Path" else "/u"

        @app.get(path)
        def handler(v: Annotated[int | str, marker]):
            return {"v": v}

        url = "/u/123" if kind == "Path" else "/u?v=123"
        if lib is fastapi:
            return TestClient(app).get(url).json()
        with LiveServerTestClient(app) as client:
            return client.get(url).json()

    expected, actual = both(run)
    assert actual == expected == {"v": 123}


def test_non_finite_floats_in_streams():
    def bodies(lib):
        sse = lib.sse if lib is fastrapi else __import__("fastapi.sse", fromlist=["x"])
        app = app_for(lib)

        @app.get("/lines")
        async def lines() -> AsyncIterable[dict]:
            yield {"x": float("nan"), "y": [float("inf")], "z": {"k": float("-inf")}}

        @app.get("/events", response_class=sse.EventSourceResponse)
        async def events():
            yield {"x": float("nan")}
            yield sse.ServerSentEvent(data={"y": [float("inf")]})

        def fetch(client):
            return client.get("/lines").text, client.get("/events").text

        if lib is fastapi:
            return fetch(TestClient(app))
        with LiveServerTestClient(app) as client:
            return fetch(client)

    expected, actual = both(bodies)
    assert actual == expected


def test_solved_dependency_exported():
    from fastapi.dependencies.utils import SolvedDependency as expected
    from fastrapi.dependencies.utils import SolvedDependency as actual

    assert actual.__name__ == expected.__name__
    assert [f for f in actual.__dataclass_fields__] == [f for f in expected.__dataclass_fields__]
