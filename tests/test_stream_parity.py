import mimetypes
from collections.abc import AsyncIterator, Iterator

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import BaseModel

import fastrapi
from conftest import LiveServerTestClient


class Item(BaseModel):
    name: str
    score: float = 1.5


def build(lib):
    app = lib.FastAPI() if lib is fastapi else lib.FastrAPI()
    sse = __import__(f"{lib.__name__}.sse", fromlist=["_"])

    @app.get("/jsonl-typed")
    def jsonl_typed() -> Iterator[Item]:
        yield {"name": "a", "extra": 1}
        yield Item(name="é")

    @app.get("/jsonl-plain")
    async def jsonl_plain():
        yield {"name": "a", "values": [1, 2.5, None], "nan": float("nan")}
        yield "text"

    @app.get("/sse-typed", response_class=sse.EventSourceResponse)
    async def sse_typed() -> AsyncIterator[Item]:
        yield {"name": "c"}

    @app.get("/sse-plain", response_class=sse.EventSourceResponse)
    def sse_plain():
        yield {"a": 1, "b": "ü\nline"}
        yield Item(name="m")

    @app.get("/sse-events", response_class=sse.EventSourceResponse)
    async def sse_events():
        yield sse.ServerSentEvent(data=Item(name="d"), event="update", id="1")
        yield sse.ServerSentEvent(data={"k": [1, 2]}, retry=10)
        yield sse.ServerSentEvent(raw_data="raw", comment="note")

    return app


PATHS = ["/jsonl-typed", "/jsonl-plain", "/sse-typed", "/sse-plain", "/sse-events"]


@pytest.fixture(scope="module")
def clients():
    with LiveServerTestClient(build(fastrapi)) as actual:
        yield TestClient(build(fastapi)), actual


@pytest.mark.parametrize("path", PATHS)
def test_stream_matches_fastapi(clients, path):
    expected_client, actual_client = clients
    expected, actual = expected_client.get(path), actual_client.get(path)
    assert actual.status_code == expected.status_code
    assert actual.headers["content-type"] == expected.headers["content-type"]
    assert actual.content == expected.content


@pytest.mark.parametrize(
    "name",
    ["a.py", "A.PY", "x.tar.gz", "x.tgz", "x.svgz", ".bashrc", "..hidden.txt", "noext", "photo.JPEG", "x.Z", "https://h/x.png", "x.unknownext"],
)
def test_file_response_media_type(name):
    expected = mimetypes.guess_type(name)[0] or "application/octet-stream"
    assert fastrapi.responses.FileResponse("p", filename=name).media_type == expected
