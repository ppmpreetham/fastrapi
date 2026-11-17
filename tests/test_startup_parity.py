"""Registration-time and OpenAPI-generation parity with FastAPI 0.162 (no per-request behaviour except the status code)."""

import json
import warnings

import fastapi
import pytest
from fastapi.testclient import TestClient

import fastrapi
from conftest import LiveServerTestClient


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def error_of(call):
    with pytest.raises(Exception) as caught:
        call()
    return type(caught.value), str(caught.value)


def both(run):
    assert run(fastrapi) == run(fastapi)


def test_telemetry_must_be_a_mapping():
    both(lambda lib: error_of(lambda: new_app(lib, telemetry=5)))


@pytest.mark.parametrize("method", ["get", "post", "put", "delete", "patch", "options", "head", "trace"])
def test_route_decorators_reject_unknown_keyword_arguments(method):
    def run(lib):
        return error_of(lambda: getattr(new_app(lib), method)("/x", foo=1)), error_of(
            lambda: getattr(lib.APIRouter(), method)("/x", foo=1)
        )

    both(run)


def test_jsonl_endpoint_keeps_status_code():
    def build(lib):
        app = new_app(lib)

        @app.get("/j", status_code=202)
        def j():
            yield {"a": 1}

        return app

    with LiveServerTestClient(build(fastrapi)) as actual:
        got = actual.get("/j")
    expected = TestClient(build(fastapi)).get("/j")
    assert got.status_code == expected.status_code == 202


def test_duplicate_operation_id_warns():
    def run(lib):
        app = new_app(lib)

        @app.get("/a", operation_id="d1")
        def d1(): ...

        @app.get("/b", operation_id="d1")
        def d2(): ...

        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            app.openapi()
        return [(w.category, str(w.message)) for w in caught]

    both(run)


def test_body_schema_lists_endpoint_fields_before_dependency_fields():
    def run(lib):
        app = new_app(lib)

        def dep(o: int = lib.Body(), b: int = lib.Body()):
            return o

        @app.post("/p")
        def p(d=lib.Depends(dep), c: int = lib.Body()): ...

        body = app.openapi()["components"]["schemas"]["Body_p_p_post"]
        return list(body["properties"]), body["required"]

    both(run)


def test_bare_dict_return_annotation_is_documented():
    def run(lib):
        app = new_app(lib)

        @app.get("/d")
        def d() -> dict: ...

        @app.get("/x", responses={200: {"description": "x", "content": {"text/plain": {}}}})
        def x() -> dict: ...

        return json.dumps(app.openapi()["paths"], sort_keys=True)

    both(run)


def test_callback_only_app_has_no_validation_components():
    def run(lib):
        router = lib.APIRouter()

        @router.post("/cb")
        def cb(q: int): ...

        app = new_app(lib)

        @app.post("/x", callbacks=router.routes)
        def x(): ...

        return app.openapi().get("components")

    both(run)
