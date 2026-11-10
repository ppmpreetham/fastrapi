import asyncio
import dataclasses
import decimal
import enum
import inspect
import io
import os
import pathlib
import tempfile
import typing
from collections.abc import AsyncIterator
from typing import Annotated

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import BaseModel

import fastrapi
from conftest import LiveServerTestClient


def app_for(lib):
    return lib.FastAPI() if lib is fastapi else lib.FastrAPI()


def module(lib, name):
    return __import__(f"{lib.__name__}.{name}", fromlist=["_"])


def both(build):
    return build(fastapi), build(fastrapi)


def test_frontend_check_dir(monkeypatch):
    missing = os.path.join(tempfile.mkdtemp(), "missing")
    for lib in (fastapi, fastrapi):
        assert inspect.signature(app_for(lib).frontend).parameters["check_dir"].default == "auto"

    def error(lib):
        with pytest.raises(RuntimeError) as info:
            app_for(lib).frontend("/", directory=missing)
        return str(info.value)

    assert error(fastrapi) == error(fastapi)
    monkeypatch.setenv("FASTAPI_ENV", "development")
    for lib in (fastapi, fastrapi):
        with pytest.warns(UserWarning, match="does not exist"):
            app_for(lib).frontend("/", directory=missing)


def test_upload_file_size_after_write():
    async def size(lib):
        upload = lib.UploadFile(file=io.BytesIO(b"hello"), size=5, filename="a")
        await upload.seek(5)
        await upload.write(b"x")
        return upload.size

    assert asyncio.run(size(fastrapi)) == asyncio.run(size(fastapi))


def test_decimal_parameter_schema():
    def schema(lib):
        app = app_for(lib)

        @app.get("/d")
        def endpoint(v: decimal.Decimal = decimal.Decimal("1.5")):
            pass

        return app.openapi()["paths"]["/d"]["get"]["parameters"][0]["schema"]

    expected, actual = both(schema)
    assert actual == expected


def test_default_type():
    assert isinstance(module(fastrapi, "datastructures").DefaultType, typing.TypeVar)


class Color(enum.Enum):
    red = "r"


@dataclasses.dataclass
class Point:
    x: int


class Model(BaseModel):
    a: int = 1


@pytest.mark.parametrize(
    ("value", "kwargs"),
    [
        ({1: "a", None: "b", 2.5: "c"}, {}),
        ({"_sa_state": 1, "keep": 2}, {}),
        ({"_sa_state": 1, "keep": 2}, {"sqlalchemy_safe": False}),
        ({"a": 1, "b": None, "c": 3}, {"include": ["a", "b"], "exclude_none": True}),
        ({"a": 1, "b": 2}, {"exclude": {"b"}}),
        ([Color.red, Point(1), Model(), pathlib.PurePosixPath("/x"), {1, 2}], {}),
        ({"when": decimal.Decimal("1.5")}, {"custom_encoder": {decimal.Decimal: str}}),
    ],
)
def test_jsonable_encoder(value, kwargs):
    expected, actual = both(lambda lib: module(lib, "encoders").jsonable_encoder(value, **kwargs))
    assert actual == expected


def test_jsonable_encoder_float_and_bytes():
    for lib in (fastapi, fastrapi):
        encoder = module(lib, "encoders").jsonable_encoder
        assert encoder(float("nan")) != encoder(float("nan"))
        with pytest.raises(UnicodeDecodeError):
            encoder(b"\xff")


def test_dependency_utils_helpers():
    async def endpoint() -> AsyncIterator[int]:
        yield 1

    class Field:
        validation_alias = None
        alias = "x"

    for lib in (fastapi, fastrapi):
        utils = module(lib, "dependencies.utils")
        assert utils.get_typed_return_annotation(endpoint) == AsyncIterator[int]
        assert utils.get_stream_item_type(AsyncIterator[int]) is int
        assert utils.get_stream_item_type(int) is None
        assert utils.get_validation_alias(Field()) == "x"
        utils.ensure_multipart_is_installed()


def test_file_response_media_type(tmp_path):
    path = tmp_path / "script.py"
    path.write_text("x")
    expected, actual = both(lambda lib: lib.responses.FileResponse(str(path)).media_type)
    assert actual == expected


def test_security_scheme_names():
    def names(lib):
        security = module(lib, "security")
        return [
            security.HTTPBearer().scheme_name,
            security.HTTPBasic().scheme_name,
            security.HTTPDigest().scheme_name,
            security.APIKeyHeader(name="k").scheme_name,
            security.APIKeyQuery(name="k").scheme_name,
            security.APIKeyCookie(name="k").scheme_name,
            security.OAuth2().scheme_name,
            security.OAuth2PasswordBearer(tokenUrl="t").scheme_name,
            security.OpenIdConnect(openIdConnectUrl="/o").scheme_name,
        ]

    expected, actual = both(names)
    assert actual == expected


def test_request_body_outside_loop():
    request = module(fastrapi, "requests").Request({"type": "http", "method": "GET", "path": "/", "headers": []})
    assert asyncio.run(_await(request.body())) == b""


async def _await(awaitable):
    return await awaitable


def live_app(lib, directory):
    app = app_for(lib)
    security = module(lib, "security")
    templates = module(lib, "templating").Jinja2Templates(directory=directory)
    app.mount("/st", module(lib, "staticfiles").StaticFiles(directory=directory), name="st")
    sse_response = (
        __import__("fastapi.sse", fromlist=["_"]).EventSourceResponse
        if lib is fastapi
        else fastrapi.sse.EventSourceResponse
    )

    class Text(str):
        pass

    class Item(BaseModel):
        name: str

    @app.get("/text")
    def text():
        return Text("abc")

    @app.get("/path")
    def path():
        return app.url_path_for("text")

    @app.get("/get-body")
    async def get_body(request: lib.Request):
        return {"body": (await request.body()).decode()}

    @app.post("/post-body")
    async def post_body(request: lib.Request):
        return {"body": (await request.body()).decode()}

    @app.get("/digest")
    def digest(credentials=lib.Depends(security.HTTPDigest(auto_error=False))):
        return credentials and {"scheme": credentials.scheme, "credentials": credentials.credentials}

    @app.get("/oauth2")
    def oauth2(token=lib.Depends(security.OAuth2(auto_error=False))):
        return {"token": token}

    @app.get("/oidc")
    def oidc(token=lib.Depends(security.OpenIdConnect(openIdConnectUrl="/o", auto_error=False))):
        return {"token": token}

    @app.post("/form")
    def form(data: Annotated[security.OAuth2PasswordRequestForm, lib.Depends()]):
        return {"scopes": data.scopes}

    @app.get("/template")
    def template(request: lib.Request):
        return templates.TemplateResponse(request, "page.html", {"x": 1})

    @app.get("/events", response_class=sse_response)
    async def events():
        yield {"a": 1.5, "é": [1, None]}

    @app.get("/typed-events", response_class=sse_response)
    async def typed_events() -> AsyncIterator[Item]:
        yield {"name": "c"}

    return app


def observe(client):
    auth = {"Authorization": "Digest abc"}
    custom = {"Authorization": "Custom tok"}
    calls = [
        client.get("/text"),
        client.get("/path"),
        client.get("/get-body"),
        client.post("/post-body"),
        client.get("/digest", headers=auth),
        client.get("/digest", headers=custom),
        client.get("/oauth2", headers=custom),
        client.get("/oauth2"),
        client.get("/oidc", headers=custom),
        client.post("/form", data={"username": "u", "password": "p", "scope": "a  b"}),
        client.get("/template"),
        client.get("/events"),
        client.get("/typed-events"),
        client.get("/st/missing.txt"),
        client.post("/st/page.html"),
    ]
    return [(response.status_code, response.content) for response in calls]


def test_live_surface(tmp_path):
    (tmp_path / "page.html").write_text("hi {{ x }}")
    expected = observe(TestClient(live_app(fastapi, str(tmp_path))))
    with LiveServerTestClient(live_app(fastrapi, str(tmp_path))) as client:
        assert observe(client) == expected
