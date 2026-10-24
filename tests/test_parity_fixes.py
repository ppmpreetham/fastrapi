from fastrapi import FastrAPI


def test_docs_pages_load_the_served_spec(client, app):
    @app.get("/a")
    def a():
        return {}

    docs = client.get("/docs").text
    assert "url: '/openapi.json'" in docs
    assert "<title>FastAPI - Swagger UI</title>" in docs
    assert 'spec-url="/openapi.json"' in client.get("/redoc").text


def test_swagger_oauth2_redirect_page(client, app):
    assert "oauth2RedirectUrl: window.location.origin + '/docs/oauth2-redirect'" in client.get("/docs").text
    response = client.get("/docs/oauth2-redirect")
    assert response.status_code == 200
    assert "swaggerUIRedirectOauth2" in response.text


def test_request_method(client, app):
    from fastrapi import Request

    @app.post("/m")
    def m(request: Request):
        return {"method": request.method}

    assert client.post("/m").json() == {"method": "POST"}


def test_version():
    import fastrapi

    assert isinstance(fastrapi.__version__, str)


def test_oauth2_classes_take_fastapi_argument_names():
    from fastrapi.security import OAuth2AuthorizationCodeBearer, OAuth2PasswordBearer

    assert OAuth2PasswordBearer(tokenUrl="/token").token_url == "/token"
    assert OAuth2PasswordBearer(token_url="/token").token_url == "/token"
    bearer = OAuth2AuthorizationCodeBearer(
        authorizationUrl="/auth", tokenUrl="/token", refreshUrl="/refresh"
    )
    assert (bearer.authorization_url, bearer.token_url, bearer.refresh_url) == (
        "/auth",
        "/token",
        "/refresh",
    )


def test_password_bearer_refresh_url_in_openapi(client, app):
    from fastrapi import Depends
    from fastrapi.security import OAuth2PasswordBearer

    scheme = OAuth2PasswordBearer(tokenUrl="/token", refreshUrl="/refresh")

    @app.get("/me")
    def me(token: str = Depends(scheme)):
        return {}

    flow = client.get("/openapi.json").json()["components"]["securitySchemes"]["OAuth2"]
    assert flow["flows"]["password"] == {"tokenUrl": "/token", "refreshUrl": "/refresh", "scopes": {}}
