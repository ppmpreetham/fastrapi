"""Contract tests for the small FastAPI compatibility modules.

These deliberately test boundaries and equivalence classes rather than merely
checking that a name can be imported.  The native implementation is expected
to keep these contracts even when its internal representation changes.
"""

import asyncio
import importlib
from types import SimpleNamespace

import pytest


def test_response_header_invariants_across_statuses():
    from fastrapi.responses import PlainTextResponse

    for status, has_length in [(200, True), (201, True), (204, False), (304, False)]:
        response = PlainTextResponse("abc", status_code=status)
        assert response.body == b"abc"
        assert ("content-length" in response.headers) is has_length
        assert response.headers.get("content-type") == "text/plain; charset=utf-8"


@pytest.mark.parametrize("value, expected", [(None, b""), (b"raw", b"raw"), ("é", "é".encode())])
def test_response_render_type_matrix(value, expected):
    from fastrapi import Response

    response = Response(value)
    assert response.render(value) == expected


def test_response_cookie_options_and_invalid_samesite():
    from fastrapi import Response

    response = Response("ok")
    response.set_cookie("sid", "x", max_age=30, secure=True, httponly=True, samesite="strict")
    cookie = response.headers.get("set-cookie")
    assert "sid=x" in cookie and "Max-Age=30" in cookie
    assert "Secure" in cookie and "HttpOnly" in cookie and "SameSite=strict" in cookie
    with pytest.raises(AssertionError):
        response.set_cookie("bad", "x", samesite="invalid")


def test_request_streams_chunks_once_and_caches_body():
    from fastrapi.request import Request

    async def exercise():
        messages = iter(
            [
                {"type": "http.request", "body": b"ab", "more_body": True},
                {"type": "http.request", "body": bytearray(b"cd"), "more_body": False},
            ]
        )
        calls = 0

        async def receive():
            nonlocal calls
            calls += 1
            return next(messages)

        request = Request(
            {"type": "http", "method": "POST", "path": "/", "headers": [(b"content-length", b"4")]},
            receive,
        )
        assert await request.body() == b"abcd"
        assert await request.body() == b"abcd"
        assert calls == 2

    asyncio.run(exercise())


def test_request_read_methods_and_mapping_helpers():
    from fastrapi.request import Request

    request = Request({"type": "http", "method": "GET", "path": "/x", "query_string": b"a=1&a=2"})
    body = request.body()
    body = asyncio.run(body) if hasattr(body, "__await__") else body
    assert body == b""
    assert request.query_params.getlist("a") == ["1", "2"]
    request["marker"] = 7
    assert request.get("marker") == 7 and "marker" in request
    assert request.get("missing", 9) == 9
    assert len(request) >= 4


@pytest.mark.parametrize(
    "path, request_path, method, expected",
    [
        ("/users/{user_id}", "/users/42", "GET", "FULL"),
        ("/users/{user_id}", "/users/42", "POST", "PARTIAL"),
        ("/users/{user_id}", "/other/42", "GET", "NONE"),
        ("/files/{rest:path}", "/files/", "GET", "FULL"),
    ],
)
def test_route_matches_partition(path, request_path, method, expected):
    from fastrapi.routing import APIRoute, Match

    def endpoint():
        """endpoint summary"""

    route = APIRoute(path, endpoint, methods=["GET"])
    match, scope = route.matches({"type": "http", "path": request_path, "method": method})
    assert match is getattr(Match, expected)
    if expected == "FULL":
        assert scope["path_params"]


def test_route_helpers_preserve_context_and_exclusions():
    from fastrapi.routing import APIRoute, RouteContext, iter_route_contexts

    def endpoint():
        pass

    hidden = APIRoute("/hidden", endpoint, include_in_schema=False)
    visible = APIRoute("/visible/{item}", endpoint, methods=["GET", "POST"])
    contexts = list(iter_route_contexts([hidden, visible]))
    assert all(isinstance(context, RouteContext) for context in contexts)
    assert contexts[0].route is hidden
    assert contexts[1].param_names == ["item"]
    assert contexts[1].methods == ["GET", "POST"]


def test_staticfiles_directory_packages_and_validation(tmp_path):
    from fastrapi.staticfiles import StaticFiles

    static = StaticFiles(directory=str(tmp_path), html=True, check_dir=True, follow_symlink=True)
    assert static.directory == str(tmp_path)
    assert static.html and static.follow_symlink
    with pytest.raises(RuntimeError):
        StaticFiles(directory=str(tmp_path / "missing"))
    no_check = StaticFiles(directory=str(tmp_path / "missing"), check_dir=False)
    assert no_check.directory.endswith("missing")


@pytest.mark.parametrize("kwargs", [{"data": {"x": 1}}, {"raw_data": "data: x\n"}, {"comment": "hello"}])
def test_sse_model_dump_round_trip_and_formatting(kwargs):
    from fastrapi.sse import ServerSentEvent, format_sse_event

    event = ServerSentEvent(**kwargs)
    dumped = event.model_dump()
    restored = ServerSentEvent.model_validate(dumped)
    assert restored.model_dump() == dumped
    wire = format_sse_event(data_str="a\nb", comment="note", retry=0)
    assert wire == b": note\ndata: a\ndata: b\nretry: 0\n\n"


@pytest.mark.parametrize("bad", [{"data": "x", "raw_data": "y"}, {"retry": -1}, {"event": "a\nb"}, {"id": "a\r"}])
def test_sse_rejects_ambiguous_or_invalid_fields(bad):
    from fastrapi.sse import ServerSentEvent

    with pytest.raises(ValueError):
        ServerSentEvent(**bad)


def test_dependency_helpers_capture_cache_scopes_and_path_names():
    from fastrapi import Depends, Security
    from fastrapi.dependencies import get_dependant, get_path_param_names

    def dep():
        return 1

    def secured_dep():
        return 2

    def endpoint(value=Depends(dep), secured=Security(secured_dep, scopes=["read"])):
        return value + secured

    dependant = get_dependant(endpoint, path="/items/{item_id}", path_param_names=["item_id"], security_scopes=["outer"])
    assert dependant.call is endpoint
    assert len(dependant.dependencies) == 2
    assert dependant.dependencies[0].use_cache is True
    security_scopes = getattr(dependant.dependencies[1], "security_scopes", None)
    assert security_scopes == ["read"] or getattr(dependant.dependencies[1], "scopes", None) == ["read"]
    assert get_path_param_names("/a/{x}/b/{tail:path}/{x}") == ["x", "tail", "x"]


def test_security_base_and_handler_contracts():
    from fastrapi.exception_handlers import http_exception_handler
    from fastrapi.security.base import APIKeyBase, HTTPBase, SecurityBase
    from fastrapi import HTTPException

    assert issubclass(APIKeyBase, object) and issubclass(HTTPBase, object) and issubclass(SecurityBase, object)
    response = http_exception_handler(None, HTTPException(418, detail={"reason": "teapot"}))
    assert response.status_code == 418 and response.body == b'{"detail":{"reason":"teapot"}}'


def test_openapi_helpers_models_docs_and_route_filters():
    from fastrapi.openapi.constants import METHODS_WITH_BODY, REF_TEMPLATE
    from fastrapi.openapi.docs import get_redoc_html, get_swagger_ui_html
    from fastrapi.openapi.models import Info, OpenAPI
    from fastrapi.openapi.utils import get_openapi
    from fastrapi.routing import APIRoute

    assert REF_TEMPLATE == "#/components/schemas/{model}"
    assert set(METHODS_WITH_BODY) == {"GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"}
    assert Info and OpenAPI

    def listed():
        """listed summary"""

    routes = [APIRoute("/listed", listed, methods=["GET"]), APIRoute("/hidden", listed, include_in_schema=False)]
    spec = get_openapi(title="T", version="1", routes=routes, tags=[{"name": "x"}], external_docs={"url": "/docs"})
    assert spec["info"] == {"title": "T", "version": "1"}
    assert set(spec["paths"]) == {"/listed"}
    assert spec["paths"]["/listed"]["get"]["summary"] == "listed summary"
    assert b"/openapi.json" in get_swagger_ui_html(openapi_url="/openapi.json", title="Docs").body
    assert b"ReDoc" in get_redoc_html(openapi_url="/openapi.json", title="Docs").body


def test_compatibility_modules_and_function_behavior():
    concurrency = importlib.import_module("fastrapi.concurrency")
    utils = importlib.import_module("fastrapi.utils")
    params = importlib.import_module("fastrapi.param_functions")

    assert asyncio.run(concurrency.run_in_threadpool(lambda: 3)) == 3
    async def collect():
        return [item async for item in concurrency.iterate_in_threadpool(iter([1, 2, 3]))]

    assert asyncio.run(collect()) == [1, 2, 3]
    assert utils.is_body_allowed_for_status_code(200)
    assert not utils.is_body_allowed_for_status_code(204)
    assert utils.deep_dict_update({"a": {"x": 1}}, {"a": {"y": 2}}) == {"a": {"x": 1, "y": 2}}
    assert params.Query(alias="q", ge=1).alias == "q"
    assert params.Body(embed=True).embed is True


@pytest.mark.parametrize(
    "module",
    [
        "fastrapi.applications",
        "fastrapi.logger",
        "fastrapi.types",
        "fastrapi.websockets",
        "fastrapi.middleware.asyncexitstack",
    ],
)
def test_compatibility_module_paths_import(module):
    assert importlib.import_module(module)
