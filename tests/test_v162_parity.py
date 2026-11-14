from typing import Annotated

import fastapi
import pytest
from fastapi.testclient import TestClient

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
