import pytest

from fastrapi import Depends, HTTPException, Query


def test_method_not_allowed_has_allow_header(client, app):
    @app.get("/only-get")
    def only_get():
        return {"ok": True}

    response = client.post("/only-get")
    assert response.status_code == 405
    assert response.headers["allow"] == "GET, HEAD"


def test_http_exception_detail_is_structured(client, app):
    @app.get("/default-error")
    def default_error():
        raise HTTPException(status_code=500)

    response = client.get("/default-error")
    assert response.json() == {"detail": "Internal Server Error"}


def test_invalid_json_uses_validation_error_shape(client, app):
    from pydantic import BaseModel

    class Payload(BaseModel):
        value: int

    @app.post("/json")
    def json_endpoint(payload: Payload):
        return payload

    response = client.post("/json", content="{")
    assert response.status_code == 422
    assert response.json()["detail"][0]["type"] == "json_invalid"


def test_async_exception_handler_and_request_validation_handler(client, app):
    from fastrapi.exceptions import RequestValidationError

    @app.exception_handler(HTTPException)
    async def http_error_handler(request, exc):
        return {"handled": exc.status_code}

    @app.exception_handler(RequestValidationError)
    def validation_handler(request, exc):
        return {"handled": len(exc.errors())}

    @app.get("/async-error")
    async def async_error():
        raise HTTPException(status_code=418)

    @app.get("/needs-int")
    def needs_int(value: int):
        return {"value": value}

    assert client.get("/async-error").json() == {"handled": 418}
    assert client.get("/needs-int", params={"value": "nope"}).json() == {"handled": 1}


def test_query_pydantic_model(client, app):
    from pydantic import BaseModel

    class Filters(BaseModel):
        limit: int = 10
        tags: list[str] = []

    @app.get("/items")
    def items(filters: Filters = Query()):
        return filters.model_dump()

    response = client.get("/items?limit=2&tags=a&tags=b")
    assert response.json() == {"limit": 2, "tags": ["a", "b"]}


def test_generic_optional_list_and_orm_response_models(client, app):
    from pydantic import BaseModel, ConfigDict

    class Item(BaseModel):
        model_config = ConfigDict(from_attributes=True)
        name: str

    class Row:
        name = "one"

    @app.get("/items", response_model=list[Item])
    def items():
        return [Row()]

    @app.get("/maybe", response_model=Item | None)
    def maybe():
        return None

    assert client.get("/items").json() == [{"name": "one"}]
    assert client.get("/maybe").status_code == 204


def test_oauth_form_class_dependency_multipart(client, app):
    from fastrapi.security import OAuth2PasswordRequestForm

    @app.post("/token")
    def token(form=Depends(OAuth2PasswordRequestForm)):
        return {"username": form.username, "password": form.password}

    response = client.post(
        "/token",
        files={"username": (None, "alice"), "password": (None, "secret")},
    )
    assert response.json() == {"username": "alice", "password": "secret"}
