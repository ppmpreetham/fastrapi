import dataclasses
import datetime
import enum
import uuid
from collections.abc import Sequence
from typing import Annotated, Literal, Optional

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import BaseModel, Field

import fastrapi
from conftest import LiveServerTestClient


class Level(enum.IntEnum):
    low = 1
    high = 2


class Filters(BaseModel):
    tag: list[str] = []
    limit: int = 10


class Tokens(BaseModel):
    x_token: str
    count: int = 0


class Login(BaseModel):
    username: str
    password: str


class Item(BaseModel):
    name: str
    price: float


@dataclasses.dataclass
class Point:
    x: int
    y: int


def build(lib):
    app = lib.FastAPI()
    Query, Header, Cookie, Body, Form, Path = lib.Query, lib.Header, lib.Cookie, lib.Body, lib.Form, lib.Path

    @app.get("/scalars")
    def scalars(
        a: int,
        b: Annotated[int, Query(gt=5)] = 6,
        c: Annotated[list[int], Query()] = [],
        u: Optional[uuid.UUID] = None,
        lit: Literal["x", "y"] = "x",
        day: Optional[datetime.date] = None,
        level: Level = Level.low,
        big: int = 0,
        flag: bool = False,
        ratio: float = 0.5,
    ):
        return {
            "a": a, "b": b, "c": c, "u": str(u), "lit": lit, "day": str(day),
            "level": level, "big": big, "flag": flag, "ratio": ratio,
        }

    @app.get("/sequences")
    def sequences(
        t: Annotated[tuple[int, ...], Query()] = (),
        f: Annotated[frozenset[str], Query()] = frozenset(),
        s: Annotated[Sequence[int], Query()] = (),
    ):
        return {"t": list(t), "f": sorted(f), "s": list(s)}

    @app.get("/required")
    def required(q: Annotated[str, Query()], h: Annotated[str, Header()]):
        return {"q": q, "h": h}

    @app.get("/aliases")
    def aliases(
        item_id: Annotated[Optional[str], Query(alias="item-id")] = None,
        x_token: Annotated[Optional[str], Header()] = None,
        raw_name: Annotated[Optional[str], Header(convert_underscores=False)] = None,
        picked: Annotated[Optional[int], Query(validation_alias="pick")] = None,
    ):
        return {"item_id": item_id, "x_token": x_token, "raw_name": raw_name, "picked": picked}

    @app.get("/factory")
    def factory(items: Annotated[list[int], Query(default_factory=list)]):
        items.append(len(items))
        return items

    @app.get("/mutable")
    def mutable(items: Annotated[list[int], Query()] = [1]):
        items.append(2)
        return items

    @app.get("/query-model")
    def query_model(filters: Annotated[Filters, Query()]):
        return filters

    @app.get("/header-model")
    def header_model(tokens: Annotated[Tokens, Header()]):
        return tokens

    @app.get("/cookie-model")
    def cookie_model(tokens: Annotated[Tokens, Cookie()]):
        return tokens

    @app.post("/form-model")
    def form_model(login: Annotated[Login, Form()]):
        return login

    @app.post("/form-fields")
    def form_fields(name: Annotated[str, Form()], note: Annotated[str, Form()] = "none"):
        return {"name": name, "note": note}

    @app.post("/body-int")
    def body_int(value: int = Body()):
        return {"value": value}

    @app.post("/body-list")
    def body_list(values: list[int]):
        return values

    @app.post("/body-dict")
    def body_dict(values: dict[str, int]):
        return values

    @app.post("/body-dataclass")
    def body_dataclass(point: Point):
        return {"x": point.x, "y": point.y}

    @app.post("/body-embed")
    def body_embed(value: Annotated[int, Body(embed=True)]):
        return {"value": value}

    @app.post("/body-many")
    def body_many(item: Item, count: int = Body(), note: str = Body("none")):
        return {"item": item, "count": count, "note": note}

    @app.post("/body-model")
    def body_model(item: Item):
        return item

    @app.get("/path/{item_id}")
    def path(item_id: Annotated[int, Path(ge=1)]):
        return {"item_id": item_id}

    return app


@pytest.fixture(scope="module")
def clients():
    expected = TestClient(build(fastapi))
    with LiveServerTestClient(build(fastrapi)) as actual:
        yield expected, actual


def request_args(case):
    method, url, kwargs = case
    return method, url, dict(kwargs)


CASES = [
    ("get", "/scalars", {"params": {"a": "1"}}),
    ("get", "/scalars", {"params": {"a": "x", "b": "1", "c": ["1", "z"]}}),
    ("get", "/scalars", {}),
    ("get", "/scalars", {"params": {"a": " 7", "u": str(uuid.UUID(int=5)), "lit": "y", "day": "2024-02-03", "level": "2", "big": str(2**70), "flag": "yes", "ratio": "1e3"}}),
    ("get", "/scalars", {"params": {"a": "1", "u": "bad", "lit": "z", "day": "nope", "level": "9", "flag": "maybe", "ratio": "nan"}}),
    ("get", "/scalars", {"params": {"a": "1_000", "b": "+9", "c": ["007"]}}),
    ("get", "/sequences", {"params": {"t": ["1", "2"], "f": ["b", "a", "b"], "s": ["3"]}}),
    ("get", "/sequences", {"params": {"t": ["x"]}}),
    ("get", "/required", {}),
    ("get", "/required", {"params": {"q": "v"}, "headers": {"h": "w"}}),
    ("get", "/aliases", {"params": {"item-id": "a", "pick": "3"}, "headers": {"x-token": "t", "raw_name": "r"}}),
    ("get", "/aliases", {"params": {"item_id": "a", "picked": "3"}, "headers": {"x_token": "t", "raw-name": "r"}}),
    ("get", "/aliases", {"params": {"pick": "z"}}),
    ("get", "/factory", {}),
    ("get", "/factory", {}),
    ("get", "/mutable", {}),
    ("get", "/mutable", {}),
    ("get", "/query-model", {"params": {"tag": ["a", "b"], "limit": "3"}}),
    ("get", "/query-model", {"params": {"tag": "a"}}),
    ("get", "/query-model", {"params": {"limit": "x"}}),
    ("get", "/header-model", {"headers": {"x-token": "t", "count": "2"}}),
    ("get", "/header-model", {"headers": {"count": "x"}}),
    ("get", "/cookie-model", {"cookies": {"x_token": "t"}}),
    ("get", "/cookie-model", {}),
    ("post", "/form-model", {"data": {"username": "u", "password": "p"}}),
    ("post", "/form-model", {"data": {"username": "u"}}),
    ("post", "/form-fields", {"data": {"name": "n", "note": ""}}),
    ("post", "/form-fields", {"data": {"name": ""}}),
    ("post", "/form-fields", {}),
    ("post", "/body-int", {"json": 5}),
    ("post", "/body-int", {"json": "x"}),
    ("post", "/body-list", {"json": [1, "2"]}),
    ("post", "/body-list", {"json": [1, "b"]}),
    ("post", "/body-dict", {"json": {"a": "1"}}),
    ("post", "/body-dataclass", {"json": {"x": 1, "y": "2"}}),
    ("post", "/body-embed", {"json": {"value": 3}}),
    ("post", "/body-embed", {"json": 3}),
    ("post", "/body-many", {"json": {"item": {"name": "n", "price": "x"}}}),
    ("post", "/body-many", {"json": {"item": {"name": "n", "price": 1}, "count": 2}}),
    ("post", "/body-many", {"json": [1, 2]}),
    ("post", "/body-many", {"content": b"{", "headers": {"content-type": "application/json"}}),
    ("post", "/body-model", {"content": b'{"name": "a", "price": 1}', "headers": {"content-type": "text/plain"}}),
    ("post", "/body-model", {"content": b'{"name": "a", "price": 1}', "headers": {"content-type": "application/merge-patch+json"}}),
    ("post", "/body-model", {"content": b'{"name": "a"', "headers": {"content-type": "application/json"}}),
    ("post", "/body-model", {"content": b'{"name": "a", "price": 1}'}),
    ("post", "/body-model", {}),
    ("get", "/path/0", {}),
    ("get", "/path/x", {}),
    ("get", "/path/4", {}),
]


CLIENT_HEADERS = {"host", "user-agent", "accept", "accept-encoding", "connection"}


def normalized(body):
    if isinstance(body, dict):
        return {key: normalized(value) for key, value in body.items() if key not in CLIENT_HEADERS}
    if isinstance(body, list):
        return [normalized(value) for value in body]
    return body


@pytest.mark.parametrize("case", CASES, ids=[f"{m} {u} {i}" for i, (m, u, _) in enumerate(CASES)])
def test_matches_fastapi(clients, case):
    expected_client, actual_client = clients
    method, url, kwargs = request_args(case)
    expected = getattr(expected_client, method)(url, **kwargs)
    actual = getattr(actual_client, method)(url, **kwargs)
    assert (actual.status_code, normalized(actual.json())) == (expected.status_code, normalized(expected.json()))


def test_marker_classes_are_field_infos():
    from pydantic.fields import FieldInfo

    for name in ("Query", "Path", "Header", "Cookie", "Body", "Form", "File"):
        assert issubclass(getattr(fastrapi, name), FieldInfo)
    assert issubclass(fastrapi.File, fastrapi.Form) and issubclass(fastrapi.Form, fastrapi.Body)
    assert issubclass(fastrapi.Query, fastrapi.params.Param)


def test_signature_errors_raise_at_decoration():
    app = fastrapi.FastAPI()
    with pytest.raises(AssertionError):
        fastrapi.Path(default=1)
    with pytest.raises(AssertionError):

        @app.get("/items/{item_id}")
        def bad(item_id: Annotated[int, fastrapi.Query()]):
            return {}
