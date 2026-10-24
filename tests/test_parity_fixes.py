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
