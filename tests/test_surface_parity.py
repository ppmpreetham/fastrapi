import inspect
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
