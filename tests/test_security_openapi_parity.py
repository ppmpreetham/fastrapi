from typing import Annotated, Literal, Union

import fastapi
from fastapi.testclient import TestClient
from pydantic import BaseModel, Field

import fastrapi


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def security(lib):
    return __import__(f"{lib.__name__}.security", fromlist=["_"])


def both(run):
    assert run(fastrapi) == run(fastapi)


class Item(BaseModel):
    name: str


class Cat(BaseModel):
    kind: Literal["cat"]


class Dog(BaseModel):
    kind: Literal["dog"]


def test_password_form_errors_match_fastapi():
    def run(lib):
        app = new_app(lib)
        s = security(lib)

        @app.post("/form")
        def form(data: Annotated[s.OAuth2PasswordRequestForm, lib.Depends()]):
            return {"user": data.username}

        @app.post("/strict")
        def strict(data: Annotated[s.OAuth2PasswordRequestFormStrict, lib.Depends()]):
            return {"user": data.username}

        c = TestClient(app)
        return [
            (r.status_code, r.json())
            for r in (
                c.post("/form", data={"username": "u"}),
                c.post("/strict", data={"username": "u", "password": "p"}),
                c.post("/strict", data={"grant_type": "x", "username": "", "password": "p"}),
            )
        ]

    both(run)


def test_security_scopes_match_fastapi():
    def run(lib):
        s = security(lib)
        calls, seen = [], {}

        def scoped(scopes: s.SecurityScopes):
            calls.append(list(scopes.scopes))

        def inner(scopes: s.SecurityScopes):
            seen["inner"] = list(scopes.scopes)

        def outer(x=lib.Security(inner, scopes=["inner"])):
            return 1

        app = new_app(lib)

        @app.get("/cache")
        def cache(a=lib.Security(scoped, scopes=["a"]), b=lib.Security(scoped, scopes=["b"])):
            return {}

        @app.get("/chain")
        def chain(y=lib.Security(outer, scopes=["outer"])):
            return {}

        @app.get("/endpoint")
        def endpoint(ss: s.SecurityScopes, t=lib.Security(s.OAuth2PasswordBearer(tokenUrl="t"), scopes=["me"])):
            return {"scopes": ss.scopes}

        c = TestClient(app)
        c.get("/cache")
        c.get("/chain")
        return calls, seen, c.get("/endpoint", headers={"Authorization": "Bearer x"}).json()

    both(run)


def test_credentials_models_match_fastapi():
    def run(lib):
        s = security(lib)
        basic = s.HTTPBasicCredentials(username="u", password="p")
        bearer = s.HTTPAuthorizationCredentials(scheme="Bearer", credentials="t")
        return basic.model_dump(), bearer.model_dump(exclude={"scheme"}), basic.model_dump_json()

    both(run)


def test_openapi_extras_match_fastapi():
    def run(lib):
        def extra(schema):
            schema["x-callable"] = 1

        callbacks = lib.APIRouter()

        @callbacks.post("{$callback_url}/events")
        def event(body: Item):
            pass

        app = new_app(lib)

        @app.get("/extra")
        def extra_route(i: Annotated[int, lib.Query(json_schema_extra=extra)]):
            pass

        @app.post("/examples")
        def examples(item: Annotated[Item, lib.Body(openapi_examples={"one": {"summary": "s", "value": {"name": "x"}}})]):
            pass

        @app.post("/union")
        def union(pet: Annotated[Union[Cat, Dog], Field(discriminator="kind")]):
            pass

        @app.post("/callbacks", callbacks=callbacks.routes)
        def with_callbacks():
            pass

        return app.openapi()

    both(run)
