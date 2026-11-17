"""Registration-time and OpenAPI-generation parity with FastAPI 0.162 (no per-request behaviour except the status code)."""

import importlib
import json
import re
import warnings
from typing import Annotated

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


def test_url_path_for_includes_builtin_routes():
    def run(lib):
        app = new_app(lib)

        @app.get("/where")
        def where(request: lib.Request):
            return {"url": str(request.url_for("openapi"))}

        client = TestClient(app) if lib is fastapi else LiveServerTestClient(app)
        with client:
            return (
                app.url_path_for("swagger_ui_html"),
                app.url_path_for("openapi"),
                app.url_path_for("redoc_html"),
                client.get("/where").json()["url"].rsplit("/", 1)[-1],
            )

    both(run)


def test_url_path_for_runs_the_convertor():
    def run(lib):
        app = new_app(lib)

        @app.get("/n/{a:int}", name="n")
        def n(a: int):
            return a

        return app.url_path_for("n", a=3), error_of(lambda: app.url_path_for("n", a="abc"))

    both(run)


def test_on_event_warns_twice():
    def run(lib):
        app = new_app(lib)
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            app.on_event("startup")
        return len([w for w in caught if issubclass(w.category, DeprecationWarning)])

    assert run(fastrapi) == run(fastapi) == 2


def test_app_has_no_add_event_handler():
    assert not hasattr(fastrapi.FastrAPI(), "add_event_handler") and not hasattr(fastapi.FastAPI(), "add_event_handler")


def test_oauth2_password_request_forms_signature():
    from fastapi.security import OAuth2PasswordRequestForm as Expected
    from fastapi.security import OAuth2PasswordRequestFormStrict as ExpectedStrict
    from fastrapi.security import OAuth2PasswordRequestForm as Actual
    from fastrapi.security import OAuth2PasswordRequestFormStrict as ActualStrict

    for actual, expected in ((Actual, Expected), (ActualStrict, ExpectedStrict)):
        form = actual(grant_type="password", username="a", password="b")
        reference = expected(grant_type="password", username="a", password="b")
        assert hasattr(form, "scope") is hasattr(reference, "scope") is False
        assert form.scopes == reference.scopes == []
        assert error_of(lambda: actual(grant_type="password", password="b"))[0] is TypeError
        assert error_of(lambda: expected(grant_type="password", password="b"))[0] is TypeError
    assert error_of(lambda: ActualStrict(username="a", password="b"))[0] is TypeError


def test_http_exception_init_signature():
    import inspect

    def run(lib):
        return [(n, p.default) for n, p in inspect.signature(lib.HTTPException.__init__).parameters.items()]

    both(run)


def test_encoder_helper_annotations():
    import inspect

    from fastapi import encoders as expected
    from fastrapi import encoders as actual

    for name in ("decimal_encoder", "isoformat", "generate_encoders_by_class_tuples"):
        assert str(inspect.signature(getattr(actual, name))) == str(inspect.signature(getattr(expected, name)))


def test_docs_helper_signature_defaults():
    import inspect

    from fastapi.openapi import docs as expected
    from fastrapi.openapi import docs as actual

    for name in ("get_swagger_ui_html", "get_redoc_html"):
        defaults = lambda module: [p.default for p in inspect.signature(getattr(module, name)).parameters.values()]
        assert defaults(actual) == defaults(expected)


def test_function_scope_dependency_under_request_scope_generator():
    def run(lib):
        def leaf():
            yield 1

        def parent(value=lib.Depends(leaf, scope="function")):
            yield value

        app = new_app(lib)

        def register():
            @app.get("/a")
            def endpoint(value=lib.Depends(parent)):
                return value

        error_type, message = error_of(register)
        return error_type.__name__, message

    both(run)


def test_slash_redirect_inside_mount_keeps_prefix():
    sub = fastrapi.FastrAPI()

    @sub.get("/i")
    def item():
        return "i"

    app = fastrapi.FastrAPI()
    app.mount("/m", sub)
    with LiveServerTestClient(app) as client:
        response = client.get("/m/i/", follow_redirects=False)
    assert response.status_code == 307
    assert response.headers["location"] == f"http://127.0.0.1:{client.port}/m/i"


class SecurityScopes:
    pass


def test_unsupported_parameter_types_raise_fastapi_error():
    def run(lib):
        app = new_app(lib)

        def user_class():
            @app.post("/a")
            def a(scopes: Annotated[SecurityScopes, lib.Body()]): ...

        def unresolved():
            @app.get("/b")
            def b(value: "Undefined"): ...

        return error_of(user_class)[0].__name__, error_of(unresolved)[0].__name__

    both(run)


def test_route_registration_assertions():
    def run(lib):
        app = new_app(lib)
        return [
            error_of(lambda: app.add_api_route("/a", lambda: 1, dependencies=[lambda: 1])),
            error_of(lambda: app.add_api_route("/b", "not callable")),
            error_of(lambda: app.include_router(5)),
            error_of(lambda: lib.APIRouter().include_router(5)),
        ]

    both(run)


def test_trusted_host_denies_a_websocket_handshake():
    def run(lib):
        app = new_app(lib)
        app.add_middleware(
            importlib.import_module(f"{lib.__name__ if lib is fastrapi else 'starlette'}.middleware.trustedhost").TrustedHostMiddleware,
            allowed_hosts=["good.com"],
        )

        @app.websocket("/ws")
        async def ws(socket: lib.WebSocket):
            await socket.accept()

        with pytest.raises(Exception) as caught:
            with TestClient(app, base_url="http://bad.com").websocket_connect("/ws"):
                pass
        return type(caught.value).__name__, caught.value.status_code, caught.value.content

    both(run)


def _dependency_marker_app(lib):
    app = new_app(lib)

    @app.get("/two")
    def two(a: Annotated[int, lib.Depends(lambda: 1), lib.Depends(lambda: 2)]):
        return a

    @app.get("/query")
    def query(a: Annotated[int, lib.Depends(lambda: 1), lib.Query()]):
        return a

    return app


def test_last_annotated_marker_wins():
    def run(lib):
        client = TestClient(_dependency_marker_app(lib))
        return client.get("/two").json(), client.get("/query").status_code, client.get("/query?a=7").json()

    both(run)


def test_openapi_is_cached_in_openapi_schema():
    for lib in (fastrapi, fastapi):
        app = new_app(lib)
        assert app.openapi() is app.openapi() is app.openapi_schema
        app.openapi_schema = {"custom": 1}
        assert app.openapi() == {"custom": 1}


def test_openapi_key_order():
    def run(lib):
        app = new_app(lib, servers=[{"url": "http://x"}], openapi_tags=[{"name": "t"}])

        @app.post("/a", tags=["t"], deprecated=True, description="d")
        def a(q: int = 1): ...

        schema = app.openapi()
        return list(schema), list(schema["paths"]["/a"]["post"])

    both(run)


def test_bytes_and_optional_upload_schemas():
    def run(lib):
        app = new_app(lib)

        @app.post("/f")
        def f(file: bytes = lib.Query(), up: Annotated[lib.UploadFile | None, lib.File()] = None): ...

        schema = app.openapi()
        return schema["paths"]["/f"]["post"]["parameters"], schema["components"]["schemas"]

    both(run)


def test_openapi_extra_deep_merges_a_body_schema():
    def run(lib):
        app = new_app(lib)

        @app.post("/a", tags=["a"], openapi_extra={"tags": ["t"], "requestBody": {"content": {"application/json": {"schema": {"type": "string"}}}}})
        def a(item: dict): ...

        operation = app.openapi()["paths"]["/a"]["post"]
        return operation["tags"], operation["requestBody"]

    both(run)


def test_password_form_dependency_documents_a_form_body():
    def run(lib):
        app = new_app(lib)
        security = importlib.import_module(f"{lib.__name__}.security")

        @app.post("/t")
        def t(form: Annotated[security.OAuth2PasswordRequestForm, lib.Depends()]): ...

        @app.post("/s")
        def s(form: Annotated[security.OAuth2PasswordRequestFormStrict, lib.Depends()]): ...

        return app.openapi()

    both(run)


def _client(lib, app):
    return TestClient(app) if lib is fastapi else LiveServerTestClient(app)


def test_app_mounted_at_root_serves_what_the_parent_does_not():
    def run(lib):
        sub = new_app(lib)

        @sub.get("/hello")
        def hello():
            return {"h": 1}

        app = new_app(lib)

        @app.get("/own")
        def own():
            return {"o": 1}

        app.mount("/", sub)
        with _client(lib, app) as client:
            return [(path, client.get(path).status_code) for path in ("/own", "/hello", "/nope", "/docs")]

    both(run)


def _spec_urls(client, *paths):
    return [re.findall(r"url: '([^']*)'", client.get(path).text) for path in paths]


def test_docs_page_of_a_mounted_app_uses_its_mount_path():
    def run(lib):
        app = new_app(lib)
        app.mount("/sub", new_app(lib))
        with _client(lib, app) as client:
            return _spec_urls(client, "/sub/docs", "/docs")

    both(run)


def test_docs_and_openapi_are_served_under_root_path():
    def run(lib):
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            app = new_app(lib, root_path="/app")
        with _client(lib, app) as client:
            statuses = [client.get(path).status_code for path in ("/docs", "/app/docs", "/app/openapi.json")]
            return statuses, _spec_urls(client, "/docs")

    both(run)
