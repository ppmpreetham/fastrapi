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


def test_openid_connect_takes_fastapi_argument_name():
    from fastrapi.security import OpenIdConnect

    assert OpenIdConnect(openIdConnectUrl="/oidc").openid_connect_url == "/oidc"


def test_http_bearer_takes_fastapi_argument_name():
    from fastrapi.security import HTTPBearer

    assert HTTPBearer(bearerFormat="JWT").bearer_format == "JWT"


def test_upload_file_headers(client, app):
    from fastrapi import UploadFile

    @app.post("/upload")
    async def upload(file: UploadFile):
        return {"content_type": file.headers.get("content-type")}

    response = client.post("/upload", files={"file": ("a.txt", b"hi", "text/plain")})
    assert response.json() == {"content_type": "text/plain"}


def test_upload_file_headers_argument():
    import io

    from starlette.datastructures import Headers

    from fastrapi import UploadFile

    upload = UploadFile(io.BytesIO(b"x"), headers=Headers({"content-type": "image/png"}))
    assert upload.content_type == "image/png"
    assert upload.headers["content-type"] == "image/png"


def test_upload_file_file_attribute(client, app):
    from fastrapi import UploadFile

    @app.post("/upload-sync")
    def upload(file: UploadFile):
        return {"data": file.file.read().decode()}

    response = client.post("/upload-sync", files={"file": ("a.txt", b"hello", "text/plain")})
    assert response.json() == {"data": "hello"}


def test_background_tasks_list():
    from starlette.background import BackgroundTask

    from fastrapi import BackgroundTasks

    def job(a, b=0):
        return a + b

    tasks = BackgroundTasks(tasks=[BackgroundTask(job, 1, b=2)])
    tasks.add_task(job, 3)
    assert [(task.func, task.args, task.kwargs) for task in tasks.tasks] == [
        (job, (1,), {"b": 2}),
        (job, (3,), {}),
    ]


def test_middleware_submodules():
    from fastrapi.middleware.gzip import GZipMiddleware
    from fastrapi.middleware.httpsredirect import HTTPSRedirectMiddleware
    from fastrapi.middleware.trustedhost import TrustedHostMiddleware
    from fastrapi.middleware.wsgi import WSGIMiddleware

    assert all((GZipMiddleware, HTTPSRedirectMiddleware, TrustedHostMiddleware, WSGIMiddleware))


def test_default_placeholder():
    from fastrapi.datastructures import Default, DefaultPlaceholder

    placeholder = Default(0)
    assert isinstance(placeholder, DefaultPlaceholder)
    assert placeholder.value == 0
    assert not placeholder
    assert placeholder == Default(0)


def test_get_value_or_default_skips_placeholders():
    from fastrapi.datastructures import Default
    from fastrapi.utils import get_value_or_default

    first = Default(1)
    assert get_value_or_default(first, Default(2), None, 3) is None
    assert get_value_or_default(first, Default(2)) is first
