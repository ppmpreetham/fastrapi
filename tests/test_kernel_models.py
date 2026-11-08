import datetime
import enum
from typing import Optional, Union

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import BaseModel, ConfigDict, Field, field_validator

import fastrapi
from conftest import LiveServerTestClient


class Kind(str, enum.Enum):
    a = "a"
    b = "b"


class Inner(BaseModel):
    x: int
    when: Optional[datetime.datetime] = None
    kind: Kind = Kind.a
    labels: set[Kind] = set()


class Aliased(BaseModel):
    model_config = ConfigDict(validate_by_name=True)
    value: int = Field(alias="Value")


class Defaults(BaseModel):
    day: datetime.date = datetime.date(2020, 1, 1)
    tags: list[str] = []
    made: dict[str, int] = Field(default_factory=lambda: {"n": 1})


class Posted(BaseModel):
    x: int
    seen: bool = False

    def model_post_init(self, context):
        self.seen = True


class Tree(BaseModel):
    name: str
    children: list["Tree"] = []


class Outer(BaseModel):
    inner: Inner
    items: list[Inner] = []
    maybe: Optional[Inner] = None
    mapping: dict[str, Inner] = {}
    aliased: Optional[Aliased] = None
    defaults: Defaults = Defaults()
    posted: Optional[Posted] = None
    tree: Optional[Tree] = None


class Custom(BaseModel):
    x: int

    def __init__(self, **data):
        super().__init__(**data)
        self.x += 1


class Validated(BaseModel):
    x: int

    @field_validator("x")
    @classmethod
    def double(cls, value):
        return value * 2


class Loose(BaseModel):
    model_config = ConfigDict(extra="allow")
    x: int


class Refused(BaseModel):
    custom: Optional[Custom] = None
    validated: Optional[Validated] = None
    loose: Optional[Loose] = None
    either: Optional[Union[Inner, Aliased]] = None


def describe(value):
    if isinstance(value, BaseModel):
        return {
            "type": type(value).__name__,
            "set": sorted(value.model_fields_set),
            "fields": {name: describe(getattr(value, name)) for name in type(value).model_fields},
            "extra": describe(value.model_extra),
        }
    if isinstance(value, (set, frozenset)):
        return ["set", sorted(describe(item) for item in value)]
    if isinstance(value, list):
        return [describe(item) for item in value]
    if isinstance(value, dict):
        return {key: describe(item) for key, item in value.items()}
    return f"{type(value).__qualname__}:{value!r}"


def build(lib):
    app = lib.FastAPI()

    @app.post("/outer")
    def outer(body: Outer):
        return describe(body)

    @app.post("/refused")
    def refused(body: Refused):
        return describe(body)

    @app.post("/tree")
    def tree(body: Tree):
        return describe(body)

    return app


@pytest.fixture(scope="module")
def clients():
    with LiveServerTestClient(build(fastrapi)) as actual:
        yield TestClient(build(fastapi)), actual


INNER = {"x": 1, "when": "2020-01-02T03:04:05+01:00", "kind": "b", "labels": ["a", "b", "a"]}
OUTER_CASES = [
    {"inner": {"x": 1}},
    {"inner": INNER, "items": [INNER, {"x": "2"}], "maybe": None, "mapping": {"k": INNER}},
    {"inner": {"x": 1}, "maybe": INNER, "aliased": {"Value": 3}},
    {"inner": {"x": 1}, "aliased": {"value": 3}},
    {"inner": {"x": 1}, "aliased": {"value": 3, "Value": 4}},
    {"inner": {"x": 1}, "defaults": {}},
    {"inner": {"x": 1}, "defaults": {"tags": ["a"]}},
    {"inner": {"x": 1}, "posted": {"x": 1}},
    {"inner": {"x": 1}, "tree": {"name": "r", "children": [{"name": "c", "children": [{"name": "g"}]}]}},
    {"inner": {"x": 1, "extra": 1}},
    {"inner": {"x": "bad"}},
    {"inner": {"x": 1}, "items": [{"x": 1}, {"kind": "z"}]},
    {"inner": {"x": 1}, "mapping": {"k": {"x": []}}},
    {"inner": {"x": 1}, "tree": {"name": "r", "children": [{"children": []}]}},
    {"inner": {"x": 1}, "aliased": {"value": "x"}},
    {"inner": None},
    {},
]
REFUSED_CASES = [
    {"custom": {"x": 1}},
    {"validated": {"x": 2}},
    {"loose": {"x": 1, "y": 2}},
    {"either": {"x": 1}},
    {"either": {"Value": 1}},
]


@pytest.mark.parametrize("payload", OUTER_CASES)
def test_nested_models_match_fastapi(clients, payload):
    expected_client, actual_client = clients
    expected = expected_client.post("/outer", json=payload)
    actual = actual_client.post("/outer", json=payload)
    assert actual.status_code == expected.status_code, (actual.text, expected.text)
    assert actual.json() == expected.json()


@pytest.mark.parametrize("payload", REFUSED_CASES)
def test_unsupported_models_match_fastapi(clients, payload):
    expected_client, actual_client = clients
    expected = expected_client.post("/refused", json=payload)
    actual = actual_client.post("/refused", json=payload)
    assert actual.status_code == expected.status_code, (actual.text, expected.text)
    assert actual.json() == expected.json()


def test_nested_models_use_kernel():
    calls = []

    class Counted(BaseModel):
        @classmethod
        def model_construct(cls, *args, **kwargs):
            calls.append(cls.__name__)
            return super().model_construct(*args, **kwargs)

    class Leaf(Counted):
        x: int
        kind: Kind = Kind.a
        tags: set[str] = set()

    class Node(Counted):
        name: str
        children: list["Node"] = []

    class Root(Counted):
        leaf: Leaf
        leaves: list[Leaf] = []
        node: Optional[Node] = None

    app = fastrapi.FastAPI()

    @app.post("/root")
    def root(body: Root):
        return describe(body)

    payload = {"leaf": {"x": 1, "kind": "b", "tags": ["a", "a"]}, "leaves": [{"x": 2}], "node": {"name": "r", "children": [{"name": "c"}]}}
    with LiveServerTestClient(app) as client:
        response = client.post("/root", json=payload)
    calls_seen = sorted(calls)
    assert response.json() == describe(Root.model_validate(payload))
    assert calls_seen == ["Leaf", "Leaf", "Node", "Node", "Root"]
