from fastapi import Depends, Response
from fastrapi import Depends as NativeDepends, FastrAPI
from fastrapi.responses import Response as NativeResponse


def test_response_parameter_is_shared_with_dependency_and_handler(client, app):
    seen = []

    def dependency(response: Response):
        seen.append(response)
        response.headers["x-dependency"] = "yes"

    @app.get("/response", dependencies=[Depends(dependency)])
    def response_endpoint(response: Response):
        assert seen[0] is response
        response.status_code = 201
        return {"ok": True}

    result = client.get("/response")
    assert result.status_code == 201
    assert result.headers["x-dependency"] == "yes"


def test_dependency_override_dict_is_live_after_server_start(client, app):
    def original():
        return "original"

    def replacement():
        return "replacement"

    @app.get("/override")
    def endpoint(value=Depends(original)):
        return {"value": value}

    assert client.get("/override").json() == {"value": "original"}
    app.dependency_overrides[original] = replacement
    assert client.get("/override").json() == {"value": "replacement"}


def test_native_response_parameter_parity():
    app = FastrAPI()
    events = []

    def dependency(response: NativeResponse):
        events.append(response)
        response.headers["x-native"] = "yes"

    @app.get("/native", dependencies=[NativeDepends(dependency)])
    async def endpoint(response: NativeResponse):
        assert events[0] is response
        response.status_code = 202
        return {"ok": True}

    from tests.test_yield_dependencies import _get

    result = _get(app, "/native")
    assert result.status_code == 202
    assert result.headers["x-native"] == "yes"
