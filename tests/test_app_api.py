import pytest

from fastrapi import FastrAPI
from fastrapi.middleware import (
    CORSMiddleware,
    GZipMiddleware,
    SessionMiddleware,
    TrustedHostMiddleware,
)


def test_app_constructor_exposes_configured_fields():
    def unique_id(route):
        return route.__name__

    app = FastrAPI(
        debug=True,
        title="Configured",
        summary="short",
        description="long",
        version="9.9.9",
        openapi_url="/schema.json",
        docs_url=None,
        redoc_url="/redoc-ui",
        generate_unique_id_function=unique_id,
    )

    assert app.debug is True
    assert app.title == "Configured"
    assert app.summary == "short"
    assert app.description == "long"
    assert app.version == "9.9.9"
    assert app.openapi_url == "/schema.json"
    assert app.docs_url is None
    assert app.redoc_url == "/redoc-ui"
    assert app.generate_unique_id_function is unique_id


def test_route_decorators_return_original_function():
    app = FastrAPI()

    def handler():
        return {"ok": True}

    for method in [
        app.get,
        app.post,
        app.put,
        app.delete,
        app.patch,
        app.options,
        app.head,
    ]:
        assert method("/resource")(handler) is handler


def test_websocket_decorator_validates_path_and_returns_function():
    app = FastrAPI()

    def websocket_handler(ws):
        return ws

    assert app.websocket("/ws")(websocket_handler) is websocket_handler

    with pytest.raises(ValueError, match="must start with '/'"):
        app.websocket("ws")


def test_add_supported_middlewares_and_reject_unknown():
    app = FastrAPI()
    app.add_middleware(CORSMiddleware, allow_origins=["https://example.test"])
    app.add_middleware(GZipMiddleware, minimum_size=256, compresslevel=5)
    app.add_middleware(SessionMiddleware, secret_key="x" * 64)

    class UnknownMiddleware:
        pass

    with pytest.raises(ValueError, match="not supported"):
        app.add_middleware(UnknownMiddleware)


def test_middleware_classes_accept_constructor_options():
    assert CORSMiddleware(
        allow_origins=["https://example.test"],
        allow_methods=["GET"],
        allow_headers=["x-token"],
        allow_credentials=True,
        expose_headers=["x-response"],
        max_age=60,
    ) is not None
    assert GZipMiddleware(minimum_size=256, compresslevel=5) is not None
    assert TrustedHostMiddleware(
        allowed_hosts=["example.test"],
        www_redirect=False,
    ) is not None
    assert SessionMiddleware(
        secret_key="x" * 64,
        session_cookie="sid",
        max_age=None,
        path="/app",
        same_site="strict",
        https_only=True,
        domain="example.test",
    ) is not None

    with pytest.raises(TypeError):
        SessionMiddleware()
