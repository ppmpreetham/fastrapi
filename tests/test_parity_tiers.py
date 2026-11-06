import fastapi
import pytest

import fastrapi


@pytest.mark.parametrize("code", [None, "default", "2XX", 100, 200, 204, 205, 304, "201", 404])
def test_body_allowed_for_status_code(code):
    from fastapi.utils import is_body_allowed_for_status_code as expected
    from fastrapi.utils import is_body_allowed_for_status_code as actual

    assert actual(code) == expected(code)


def test_status_module_matches_fastapi():
    import warnings

    from fastrapi import status

    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        assert status.HTTP_422_UNPROCESSABLE_ENTITY == fastapi.status.HTTP_422_UNPROCESSABLE_ENTITY
    assert status.HTTP_418_IM_A_TEAPOT == 418 and status.WS_1008_POLICY_VIOLATION == 1008


def test_redirect_response_accepts_url_objects(client, app):
    from fastrapi import Request
    from fastrapi.responses import RedirectResponse

    @app.get("/target")
    def target():
        return {}

    @app.get("/redirect")
    def redirect(request: Request):
        return RedirectResponse(request.url_for("target"))

    response = client.get("/redirect", follow_redirects=False)
    assert response.status_code == 307
    assert response.headers["location"].endswith("/target")


def test_response_headers_accept_mappings(client, app):
    from types import MappingProxyType

    from fastrapi.responses import FileResponse, JSONResponse, PlainTextResponse, RedirectResponse

    headers = MappingProxyType({"x-a": "1"})
    assert JSONResponse({}, headers=headers).headers["x-a"] == "1"
    assert RedirectResponse("/x", headers=headers).headers["x-a"] == "1"
    assert FileResponse(__file__, headers=headers).headers["x-a"] == "1"

    @app.get("/mapped")
    def mapped():
        return PlainTextResponse("ok", headers=headers)

    assert client.get("/mapped").headers["x-a"] == "1"


def test_mount_name_is_positional(tmp_path):
    from fastrapi.staticfiles import StaticFiles

    app = fastrapi.FastAPI()
    app.mount("/static", StaticFiles(directory=str(tmp_path)), "static")


def test_webhooks_default_router():
    assert "webhooks" not in fastrapi.FastAPI().openapi()
    app = fastrapi.FastAPI()

    @app.webhooks.post("new-subscription")
    def new_subscription(body: dict):
        return None

    assert "new-subscription" in app.openapi()["webhooks"]
