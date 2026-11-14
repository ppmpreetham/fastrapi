import warnings
from typing import Annotated

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import BaseModel, Field

import fastrapi


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def both(run):
    assert run(fastrapi) == run(fastapi)


def raises_type_error(call):
    try:
        call()
    except TypeError:
        return True
    return False


@pytest.mark.parametrize("kwargs", [{"team": "core"}, {"openapi_prefix": "/api"}])
def test_app_rejects_unknown_keyword_arguments(kwargs):
    both(lambda lib: raises_type_error(lambda: new_app(lib, **kwargs)))


def test_root_path_emits_deprecation_warning():
    def run(lib):
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            app = new_app(lib, root_path="/api")
            new_app(lib)
        return app.root_path, [(type(w.message).__name__, str(w.message)) for w in caught]

    both(run)


def test_telemetry_config_defaults_and_overrides():
    def run(lib):
        return new_app(lib)._telemetry, new_app(lib, telemetry={"tracing": False, "exclude": ["/health"]})._telemetry

    both(run)


@pytest.mark.parametrize("marker", ["Query", "Header", "Cookie", "Path", "Body", "Form", "File"])
def test_param_functions_reject_unknown_keyword_arguments(marker):
    both(lambda lib: raises_type_error(lambda: getattr(lib, marker)(foo=1)))


@pytest.mark.parametrize("marker", ["Query", "Header", "Body", "Form"])
def test_param_functions_accept_pydantic_field_options(marker):
    def run(lib):
        info = getattr(lib, marker)(None, validate_default=True, union_mode="left_to_right", fail_fast=True)
        return info.validate_default, repr(info), info.json_schema_extra

    both(run)


def test_validate_default_validates_missing_values():
    def run(lib):
        app = new_app(lib)

        @app.get("/bad")
        def bad(d: Annotated[int, lib.Query(validate_default=True)] = "x"):
            return d

        @app.get("/coerced")
        def coerced(
            d: Annotated[int, lib.Query(validate_default=True)] = "3",
            h: Annotated[list[int], lib.Header(validate_default=True)] = ["4"],
            plain: Annotated[int, lib.Query()] = "5",
        ):
            return [d, h, plain]

        client = TestClient(app)
        bad_response = client.get("/bad")
        return (
            bad_response.status_code,
            [(e["type"], e["loc"]) for e in bad_response.json()["detail"]],
            client.get("/bad?d=2").json(),
            client.get("/coerced").json(),
        )

    both(run)


def test_field_annotations_reach_parameter_schema():
    def run(lib):
        app = new_app(lib)

        @app.get("/f")
        def f(
            q: Annotated[int, Field(gt=3, description="d")] = 5,
            r: Annotated[str, lib.Query(), Field(max_length=3, title="T")] = "x",
        ):
            return [q, r]

        client = TestClient(app)
        return app.openapi()["paths"], client.get("/f?q=1").status_code, client.get("/f?q=4&r=ab").json()

    both(run)


def test_param_models_flatten_into_parameters():
    class Tags(BaseModel):
        name: str = "n"

    class Filters(BaseModel):
        limit: int = Field(10, description="max", le=100)
        tags: list[str] = []
        nested: Tags | None = None

    class Paging(BaseModel):
        page: int

    class Headers(BaseModel):
        x_token: str
        user_agent: str | None = Field(None, deprecated=True)

    class Cookies(BaseModel):
        session_id: str = Field(title="Session")

    def run(lib):
        app = new_app(lib)

        @app.get("/single")
        def single(filters: Annotated[Filters, lib.Query()]):
            return filters

        @app.get("/combined")
        def combined(
            filters: Annotated[Filters, lib.Query()],
            paging: Annotated[Paging, lib.Query()],
            headers: Annotated[Headers, lib.Header()],
            cookies: Annotated[Cookies, lib.Cookie()],
        ):
            return [filters, paging]

        schema = app.openapi()
        return schema["paths"], schema.get("components")

    both(run)


def test_optional_param_models():
    class Filters(BaseModel):
        limit: int = 10
        tag: str | None = None

    class Strict(BaseModel):
        model_config = {"extra": "forbid"}
        limit: int = 10

    class Token(BaseModel):
        x_token: str

    def run(lib):
        app = new_app(lib)

        @app.get("/query")
        def query(filters: Annotated[Filters | None, lib.Query()] = None):
            return filters

        @app.get("/required")
        def required(filters: Annotated[Filters | None, lib.Query()]):
            return filters

        @app.get("/strict")
        def strict(filters: Annotated[Strict | None, lib.Query()] = None):
            return filters

        @app.get("/header")
        def header(token: Annotated[Token | None, lib.Header()] = None):
            return token

        client = TestClient(app)
        calls = [
            client.get("/query"),
            client.get("/query?limit=3"),
            client.get("/query?other=1"),
            client.get("/query?limit=x"),
            client.get("/required"),
            client.get("/required?tag=a"),
            client.get("/strict"),
            client.get("/strict?other=1"),
            client.get("/header"),
            client.get("/header", headers={"x-token": "t"}),
        ]
        return [(r.status_code, r.json()) for r in calls], app.openapi()["paths"]

    both(run)
