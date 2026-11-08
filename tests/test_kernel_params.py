import datetime
import decimal
import enum
import uuid
from typing import Annotated, Literal, Optional, Union

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import UUID4, Field, StringConstraints

import fastrapi
from conftest import LiveServerTestClient


class Color(str, enum.Enum):
    red = "red"
    blue = "blue"


class Plain(enum.Enum):
    one = "one"
    two = 2


class Level(enum.IntEnum):
    low = 1
    high = 2


TYPES = {
    "date": (datetime.date, ["2020-01-02", "2020-13-01", "20200102", "1577923200", "x"]),
    "datetime": (
        datetime.datetime,
        ["2020-01-02T03:04:05", "2020-01-02T03:04:05+01:00", "2020-01-02T03:04:05Z", "2020-01-02 03:04:05.1234567", "1577923200", "nope"],
    ),
    "time": (datetime.time, ["03:04:05", "03:04:05+02:00", "25:00", "03:04"]),
    "timedelta": (datetime.timedelta, ["P1DT2H", "-P1DT0.5S", "1:02:03", "3600", "-1.5", "bad"]),
    "optional_date": (Optional[datetime.date], ["2020-01-02", "bad"]),
    "color": (Color, ["red", "blue", "RED", "green"]),
    "plain": (Plain, ["one", "2", "two"]),
    "level": (Level, ["1", "2", "3", "low"]),
    "literal": (Literal["x", "y"], ["x", "y", "z"]),
    "literal_int": (Literal[1, 2], ["1", "2", "3"]),
    "multiple": (Annotated[int, Field(multiple_of=3)], ["9", "10", "x"]),
    "finite": (Annotated[float, Field(allow_inf_nan=False, gt=1)], ["2.5", "inf", "nan", "0.5", "1e3"]),
    "union": (Union[int, str], ["1", "a", "1.5"]),
    "bytes": (bytes, ["abc", ""]),
    "lower": (Annotated[str, StringConstraints(to_lower=True, min_length=2)], ["AbC", "A"]),
    "strict_int": (Annotated[int, Field(strict=True)], ["1"]),
    "bool_opt": (Optional[bool], ["yes", "off", "maybe"]),
    "uuid": (
        uuid.UUID,
        [
            "12345678-1234-5678-1234-567812345678",
            "12345678123456781234567812345678",
            "{12345678-1234-5678-1234-567812345678}",
            "urn:uuid:12345678-1234-5678-1234-567812345678",
            "12345678-1234-5678-1234-56781234567Z",
            " 12345678-1234-5678-1234-567812345678",
            "1234",
        ],
    ),
    "uuid4": (UUID4, ["9b2f4c1e-5a8d-4e3b-9f6a-1c2d3e4f5a6b", "12345678-1234-5678-1234-567812345678", "9b2f4c1e-5a8d-4e3b-cf6a-1c2d3e4f5a6b"]),
    "optional_uuid": (Optional[uuid.UUID], ["12345678-1234-5678-1234-567812345678", "x"]),
    "decimal": (decimal.Decimal, ["1.50", " 2 ", "1_000", "1e3", "-0", "NaN", "Infinity", "abc", ""]),
    "decimal_bounded": (Annotated[decimal.Decimal, Field(gt=0, le=10)], ["5", "0", "10", "10.01", "sNaN"]),
    "decimal_inf": (Annotated[decimal.Decimal, Field(allow_inf_nan=True)], ["NaN", "-Infinity", "3"]),
    "decimal_digits": (Annotated[decimal.Decimal, Field(max_digits=3, decimal_places=1)], ["12.3", "1.23"]),
}

LIST_TYPES = {
    "list_color": (list[Color], [["red", "blue"], ["red", "green"]]),
    "set_int": (set[int], [["1", "2", "1"], ["1", "x"]]),
    "list_date": (list[datetime.date], [["2020-01-02"], ["bad"]]),
    "list_literal": (list[Literal["a", "b"]], [["a", "b"], ["c"]]),
    "list_uuid": (list[uuid.UUID], [["12345678-1234-5678-1234-567812345678"], ["bad"]]),
    "list_decimal": (list[decimal.Decimal], [["1.5", "2"], ["x"]]),
}


def describe(value):
    if isinstance(value, (set, frozenset)):
        return ["set", sorted(describe(item) for item in value)]
    if isinstance(value, (list, tuple)):
        return [describe(item) for item in value]
    if isinstance(value, dict):
        return {key: describe(item) for key, item in value.items()}
    return f"{type(value).__qualname__}:{value!r}"


def build(lib):
    app = lib.FastAPI()
    for name, (annotation, _) in {**TYPES, **LIST_TYPES}.items():
        is_list = name in LIST_TYPES

        def query(v: Annotated[annotation, lib.Query()]):
            return describe(v)

        def embedded(v: Annotated[annotation, lib.Body()], w: Annotated[int, lib.Body()] = 0):
            return describe(v)

        def form(v: Annotated[annotation, lib.Form()]):
            return describe(v)

        def body(v: Annotated[annotation, lib.Body()]):
            return describe(v)

        app.get(f"/query/{name}")(query)
        app.post(f"/embedded/{name}")(embedded)
        app.post(f"/form/{name}")(form)
        if is_list:
            app.post(f"/body/{name}")(body)
    return app


@pytest.fixture(scope="module")
def clients():
    with LiveServerTestClient(build(fastrapi)) as actual:
        yield TestClient(build(fastapi)), actual


def compare(expected, actual):
    assert actual.status_code == expected.status_code, (actual.text, expected.text)
    assert actual.json() == expected.json()


SCALAR_CASES = [(name, raw) for name, (_, inputs) in TYPES.items() for raw in inputs]
LIST_CASES = [(name, raw) for name, (_, inputs) in LIST_TYPES.items() for raw in inputs]


@pytest.mark.parametrize(("name", "raw"), SCALAR_CASES, ids=[f"{n}-{r}" for n, r in SCALAR_CASES])
def test_scalar_params_match_fastapi(clients, name, raw):
    expected_client, actual_client = clients
    for method, url, kwargs in (
        ("get", f"/query/{name}", {"params": {"v": raw}}),
        ("post", f"/embedded/{name}", {"json": {"v": raw}}),
        ("post", f"/form/{name}", {"data": {"v": raw}}),
    ):
        compare(getattr(expected_client, method)(url, **kwargs), getattr(actual_client, method)(url, **kwargs))


@pytest.mark.parametrize(("name", "raw"), LIST_CASES, ids=[f"{n}-{r}" for n, r in LIST_CASES])
def test_list_params_match_fastapi(clients, name, raw):
    expected_client, actual_client = clients
    for method, url, kwargs in (
        ("get", f"/query/{name}", {"params": {"v": raw}}),
        ("post", f"/embedded/{name}", {"json": {"v": raw}}),
        ("post", f"/form/{name}", {"data": {"v": raw}}),
        ("post", f"/body/{name}", {"json": raw}),
    ):
        compare(getattr(expected_client, method)(url, **kwargs), getattr(actual_client, method)(url, **kwargs))


@pytest.mark.parametrize(
    "value",
    [1, 1.0, 2.5, True, None, "3", [1], {"a": 1}],
)
def test_embedded_json_values_match_fastapi(clients, value):
    expected_client, actual_client = clients
    for name in ("multiple", "finite", "union", "level", "plain", "literal_int", "bool_opt", "datetime", "decimal", "decimal_bounded"):
        url = f"/embedded/{name}"
        compare(expected_client.post(url, json={"v": value}), actual_client.post(url, json={"v": value}))


def test_kernel_params_skip_pydantic(monkeypatch):
    from pydantic import TypeAdapter

    calls = []
    original = TypeAdapter.validate_python

    def counted(self, *args, **kwargs):
        calls.append(args[0] if args else None)
        return original(self, *args, **kwargs)

    monkeypatch.setattr(TypeAdapter, "validate_python", counted)
    app = build(fastrapi)
    with LiveServerTestClient(app) as client:
        valid = {name: inputs[0] for name, (_, inputs) in TYPES.items() if name not in ("strict_int", "literal_int", "decimal_digits")}
        for name, raw in valid.items():
            assert client.get(f"/query/{name}", params={"v": raw}).status_code == 200, name
            assert client.post(f"/embedded/{name}", json={"v": raw}).status_code == 200, name
            assert client.post(f"/form/{name}", data={"v": raw}).status_code == 200, name
        for name, (_, inputs) in LIST_TYPES.items():
            assert client.get(f"/query/{name}", params={"v": inputs[0]}).status_code == 200, name
            assert client.post(f"/body/{name}", json=inputs[0]).status_code == 200, name
    assert calls == []


def test_rust_uuid_matches_python_uuid():
    app = fastrapi.FastAPI()

    @app.get("/u")
    def read(v: uuid.UUID):
        expected = uuid.UUID(str(v))
        return [type(v) is uuid.UUID, v == expected, hash(v) == hash(expected), v.version, v.is_safe.name, str(v)]

    with LiveServerTestClient(app) as client:
        raw = "9b2f4c1e-5a8d-4e3b-9f6a-1c2d3e4f5a6b"
        assert client.get("/u", params={"v": raw.upper()}).json() == [True, True, True, 4, "unknown", raw]
