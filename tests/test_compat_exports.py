def test_compatibility_modules_import():
    from fastrapi.dependencies import get_dependant
    from fastrapi.exception_handlers import http_exception_handler
    from fastrapi.openapi.docs import get_redoc_html, get_swagger_ui_html
    from fastrapi.openapi.models import OpenAPI
    from fastrapi.routing import APIRoute, APIRouter, Match, RouteContext
    from fastrapi.security.base import APIKeyBase, HTTPBase, SecurityBase

    assert all((get_dependant, http_exception_handler, get_redoc_html, get_swagger_ui_html))
    assert all((OpenAPI, APIRoute, APIRouter, Match, RouteContext))
    assert all((APIKeyBase, HTTPBase, SecurityBase))


def test_dependency_helper_uses_native_parser():
    from fastrapi import Depends
    from fastrapi.dependencies import get_dependant

    def dependency():
        return 1

    def endpoint(value=Depends(dependency)):
        return value

    dependant = get_dependant(endpoint)
    assert dependant.call is endpoint
    assert dependant.dependencies[0].call is dependency


def test_openapi_docs_return_native_response():
    from fastrapi.openapi.docs import get_swagger_ui_html

    response = get_swagger_ui_html(openapi_url="/openapi.json", title="Docs")
    assert response.status_code == 200
    assert b"Docs" in response.body
