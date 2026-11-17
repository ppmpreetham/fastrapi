"""Regression tests for review fixes. Each one compares fastrapi against FastAPI 0.162 in-process."""

import dataclasses
import json
from typing import Optional

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import BaseModel, ConfigDict, Field, computed_field

import fastrapi
from conftest import LiveServerTestClient


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


class RequiredNone(BaseModel):
    a: Optional[int]
    b: int = 1


class SerializationAlias(BaseModel):
    foo: int = Field(validation_alias="vfoo", serialization_alias="sfoo")


class ConfigAlias(BaseModel):
    model_config = ConfigDict(serialize_by_alias=True)
    foo: int = Field(1, serialization_alias="FOO")


class Computed(BaseModel):
    model_config = ConfigDict(extra="forbid")
    x: int

    @computed_field
    @property
    def double(self) -> int:
        return self.x * 2


class Item(BaseModel):
    id: int
    name: str = "n"
    opt: Optional[str] = None


class SubItem(Item):
    secret: str = "s"


@dataclasses.dataclass
class Point:
    a: int
    b: Optional[str] = None


class Attributes:
    def __init__(self):
        self.id = 7
        self.name = "obj"


RESPONSE_MODEL_CASES = {
    "required_none": (lambda: RequiredNone(a=None), RequiredNone, {"response_model_exclude_none": True}),
    "required_none_list": (lambda: [RequiredNone(a=None)], list[RequiredNone], {"response_model_exclude_none": True}),
    "required_none_defaults": (lambda: RequiredNone(a=None), RequiredNone, {"response_model_exclude_defaults": True}),
    "serialization_alias": (lambda: SerializationAlias(vfoo=1), SerializationAlias, {}),
    "serialization_alias_off": (lambda: SerializationAlias(vfoo=1), SerializationAlias, {"response_model_by_alias": False}),
    "serialization_alias_dict": (lambda: {"sfoo": 1}, SerializationAlias, {}),
    "config_alias_off": (lambda: ConfigAlias(), ConfigAlias, {"response_model_by_alias": False}),
    "computed_forbid": (lambda: Computed(x=2), Computed, {}),
    "computed_forbid_list": (lambda: [Computed(x=2)], list[Computed], {}),
    "fields_set": (lambda: Item(id=1, opt=None), Item, {"response_model_exclude_unset": True}),
    "fields_set_dict": (lambda: {"id": "5", "opt": None}, Item, {"response_model_exclude_unset": True}),
    "subclass": (lambda: SubItem(id=2, secret="z"), Item, {"response_model_exclude_unset": True}),
    "list_mixed": (lambda: [Item(id=1), SubItem(id=2), {"id": 3}], list[Item], {"response_model_exclude_unset": True}),
    "list_defaults": (lambda: [Item(id=1, name="n"), {"id": 3, "name": "q"}], list[Item], {"response_model_exclude_defaults": True}),
    "mapping": (lambda: {"a": Item(id=1), "b": {"id": 2}}, dict[str, Item], {"response_model_exclude_none": True}),
    "mapping_include": (lambda: {"a": Item(id=1), "b": {"id": 2}}, dict[str, Item], {"response_model_include": {"a"}}),
    "include": (lambda: Item(id=1), Item, {"response_model_include": {"id"}}),
    "exclude": (lambda: Item(id=1), Item, {"response_model_exclude": {"id"}}),
    "dataclass": (lambda: Point(a=1), Point, {"response_model_exclude_none": True}),
    "dataclass_unvalidated": (lambda: Point(a="x"), Point, {}),
    "dataclass_list": (lambda: [Point(a=1), {"a": 2}], list[Point], {}),
    "attributes": (lambda: Attributes(), Item, {}),
    "none": (lambda: None, Item, {}),
    "none_optional": (lambda: None, Optional[Item], {}),
    "invalid_dict": (lambda: {"name": 3}, Item, {}),
    "invalid_list_item": (lambda: [Item(id=1), {"id": "x"}], list[Item], {}),
    "invalid_dataclass_for_model": (lambda: Point(a=1), RequiredNone, {}),
    "invalid_scalar": (lambda: "x", int, {}),
}


def response_model_app(lib):
    app = new_app(lib)

    @app.exception_handler(lib.exceptions.ResponseValidationError)
    async def invalid(request, exc):
        try:
            body = json.loads(json.dumps(exc.body))
        except TypeError:
            body = type(exc.body).__name__
        errors = json.loads(json.dumps(exc.errors(), default=repr))
        return lib.responses.JSONResponse({"type": type(exc).__name__, "errors": errors, "body": body}, status_code=418)

    class Custom(lib.responses.JSONResponse):
        media_type = "application/x-custom"

    def add(path, content, **options):
        def sync_endpoint():
            return content()

        async def async_endpoint():
            return content()

        app.get(f"/sync/{path}", **options)(sync_endpoint)
        app.get(f"/async/{path}", **options)(async_endpoint)

    for name, (content, model, options) in RESPONSE_MODEL_CASES.items():
        add(name, content, response_model=model, **options)
    add("custom_class", lambda: SubItem(id=1), response_model=Item, response_class=Custom)
    return app


@pytest.fixture(scope="module")
def response_model_clients():
    expected = TestClient(response_model_app(fastapi))
    with LiveServerTestClient(response_model_app(fastrapi)) as actual:
        yield actual, expected


@pytest.mark.filterwarnings("ignore:Pydantic serializer warnings")
@pytest.mark.parametrize("mode", ["sync", "async"])
@pytest.mark.parametrize("name", [*RESPONSE_MODEL_CASES, "custom_class"])
def test_response_model_validates_the_return_value_itself(response_model_clients, mode, name):
    actual, expected = response_model_clients
    got, want = actual.get(f"/{mode}/{name}"), expected.get(f"/{mode}/{name}")
    assert (got.status_code, got.headers.get("content-type"), got.text) == (
        want.status_code,
        want.headers.get("content-type"),
        want.text,
    )
