import socket
import uuid

import pytest

from fastrapi import Depends, FastrAPI


def test_bind_failure_raises_oserror():
    app = FastrAPI()

    @app.get("/")
    def root():
        return {}

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as taken:
        taken.bind(("127.0.0.1", 0))
        taken.listen()
        port = taken.getsockname()[1]
        with pytest.raises(OSError, match="failed to bind"):
            app.serve("127.0.0.1", port)


def test_rate_limit_zero_is_rejected_at_registration(app):
    with pytest.raises(ValueError, match="rate_limit must be greater than 0"):

        @app.get("/limited", rate_limit=0)
        def limited():
            return {}


def test_override_is_live_for_async_handler_with_sync_dependency(client, app):
    def original():
        return "original"

    def replacement():
        return "replacement"

    @app.get("/async-override")
    async def endpoint(value=Depends(original)):
        return {"value": value}

    assert client.get("/async-override").json() == {"value": "original"}
    app.dependency_overrides[original] = replacement
    assert client.get("/async-override").json() == {"value": "replacement"}
    app.dependency_overrides.clear()
    assert client.get("/async-override").json() == {"value": "original"}


def test_uuid_path_convertor_matches_like_starlette(client, app):
    @app.get("/items/{item_id:uuid}")
    def item(item_id: str):
        return {"item_id": item_id}

    dashed = str(uuid.uuid4())
    assert client.get(f"/items/{dashed}").status_code == 200
    assert client.get(f"/items/{uuid.uuid4().hex}").status_code == 200
    assert client.get("/items/not-a-uuid").status_code == 404
    assert client.get(f"/items/{dashed}-0000").status_code == 404


def test_datastructures_submodule_exports():
    from fastrapi.datastructures import URL, FormData, UploadFile

    assert {URL.__name__, FormData.__name__, UploadFile.__name__} == {
        "URL",
        "FormData",
        "UploadFile",
    }
