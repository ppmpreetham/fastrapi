import fastapi
import fastapi.responses
import pytest
from fastapi.testclient import TestClient

import fastrapi
import fastrapi.responses


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def submodule(lib, name):
    return __import__(f"{lib.__name__}.{name}", fromlist=["_"])


def client(app):
    return TestClient(app, raise_server_exceptions=False, follow_redirects=False)


def both(run):
    assert run(fastrapi) == run(fastapi)


def test_json_response_subclasses_match_fastapi():
    def run(lib):
        r = lib.responses
        return [issubclass(cls, r.JSONResponse) for cls in (r.ORJSONResponse, r.UJSONResponse)]

    both(run)


@pytest.mark.filterwarnings("ignore::DeprecationWarning")
def test_json_response_subclasses_render():
    r = fastrapi.responses
    app = fastrapi.FastrAPI()

    @app.get("/o")
    def o():
        return r.ORJSONResponse({"a": 1}, status_code=201, headers={"x-a": "1"})

    @app.get("/u")
    def u():
        return r.UJSONResponse({"b": 2})

    c = client(app)
    assert [(x.status_code, x.json(), x.headers.get("x-a")) for x in (c.get("/o"), c.get("/u"))] == [
        (201, {"a": 1}, "1"),
        (200, {"b": 2}, None),
    ]
