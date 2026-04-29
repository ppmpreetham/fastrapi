import httpx


def test_core_http_routing_and_validation(run_server):
    server = run_server(
        """
        from typing import Optional

        from pydantic import BaseModel

        from fastrapi import Depends, FastrAPI, Header, HTTPException, Path, Query, Security
        from fastrapi import SecurityScopes, status
        from fastrapi.responses import HTMLResponse, JSONResponse, PlainTextResponse, RedirectResponse


        class User(BaseModel):
            name: str
            age: int
            email: Optional[str] = None


        class Item(BaseModel):
            id: int
            name: str
            price: float


        app = FastrAPI(title="Test API", version="1.0.0", description="test suite")


        @app.get("/health")
        def health():
            return {{"status": "ok"}}


        @app.get("/")
        def root():
            return {{"message": "Hello World"}}


        @app.get("/items/{{item_id}}")
        def get_item(item_id: int):
            return {{"item_id": item_id, "type": type(item_id).__name__}}


        @app.get("/search")
        def search(q: str, limit: int = 10):
            return {{"query": q, "limit": limit}}


        @app.post("/users")
        def create_user(user: User):
            return {{"created": user.model_dump(), "name_upper": user.name.upper()}}


        @app.put("/items/{{item_id}}")
        def update_item(item_id: int, item: Item):
            return {{"item_id": item_id, "item": item.model_dump()}}


        @app.get("/html")
        def html() -> HTMLResponse:
            return HTMLResponse("<h1>Hello HTML</h1>")


        @app.get("/json")
        def json_response() -> JSONResponse:
            return JSONResponse({{"format": "json"}}, status_code=201)


        @app.get("/text")
        def text() -> PlainTextResponse:
            return PlainTextResponse("plain text")


        @app.get("/redirect")
        def redirect() -> RedirectResponse:
            return RedirectResponse("/", status_code=307)


        @app.get("/error/{{code}}")
        def error(code: int):
            if code:
                raise HTTPException(status_code=code, detail="raised")
            return {{"code": code}}


        @app.get("/validate/query")
        def validate_query(
            age: int = Query(default=18, ge=0, le=150),
            name: str = Query(default="anonymous", min_length=2, max_length=20),
        ):
            return {{"age": age, "name": name}}


        @app.get("/validate/path/{{user_id}}")
        def validate_path(user_id: int = Path(..., ge=1, le=10)):
            return {{"user_id": user_id}}


        def current_user():
            return {{"id": 123, "name": "Ada"}}


        @app.get("/protected")
        def protected(user=Depends(current_user)):
            return {{"user": user}}


        def verify_token(token: str = Header(...)):
            if token != "valid-token":
                raise HTTPException(status_code=401, detail="invalid token")
            return token


        @app.get("/auth")
        def auth(token=Depends(verify_token)):
            return {{"token": token}}


        def verify_scopes(scopes: SecurityScopes, token: str = Header(...)):
            return {{"scopes": scopes.scopes, "token": token}}


        @app.get("/admin")
        def admin(auth=Security(verify_scopes, scopes=["admin"])):
            return {{"auth": auth}}


        @app.post("/created")
        def created():
            return JSONResponse({{"id": 1}}, status_code=status.HTTP_201_CREATED)


        @app.get("/empty")
        def empty():
            return None


        app.serve(host="127.0.0.1", port={port})
        """
    )

    assert httpx.get(f"{server}/").json() == {"message": "Hello World"}
    assert httpx.get(f"{server}/items/42").json() == {"item_id": 42, "type": "int"}
    assert httpx.get(f"{server}/items/not-int").status_code == 422
    assert httpx.get(f"{server}/search?q=fastrapi").json() == {
        "query": "fastrapi",
        "limit": 10,
    }

    user = {"name": "Grace", "age": 37, "email": "grace@example.test"}
    response = httpx.post(f"{server}/users", json=user)
    assert response.status_code == 200
    assert response.json()["created"] == user
    assert response.json()["name_upper"] == "GRACE"
    assert httpx.post(f"{server}/users", json={"name": "Grace"}).status_code == 422

    response = httpx.put(
        f"{server}/items/9",
        json={"id": 1, "name": "Widget", "price": 12.5},
    )
    assert response.status_code == 200
    assert response.json()["item"]["name"] == "Widget"

    assert "text/html" in httpx.get(f"{server}/html").headers["content-type"]
    assert httpx.get(f"{server}/json").status_code == 201
    assert httpx.get(f"{server}/text").text == "plain text"
    response = httpx.get(f"{server}/redirect", follow_redirects=False)
    assert response.status_code == 307
    assert response.headers["location"] == "/"

    assert httpx.get(f"{server}/error/404").json() == {"detail": "raised"}
    assert httpx.get(f"{server}/validate/query").json() == {
        "age": 18,
        "name": "anonymous",
    }
    assert httpx.get(f"{server}/validate/query?age=151").status_code == 422
    assert httpx.get(f"{server}/validate/path/7").json() == {"user_id": 7}
    assert httpx.get(f"{server}/validate/path/11").status_code == 422

    assert httpx.get(f"{server}/protected").json() == {"user": {"id": 123, "name": "Ada"}}
    assert httpx.get(f"{server}/auth", headers={"token": "valid-token"}).json() == {
        "token": "valid-token"
    }
    assert httpx.get(f"{server}/auth", headers={"token": "bad"}).status_code == 401
    response = httpx.get(f"{server}/admin", headers={"token": "admin-token"})
    assert response.json()["auth"]["scopes"] == ["admin"]

    assert httpx.post(f"{server}/created").status_code == 201
    assert httpx.get(f"{server}/empty").status_code == 204


def test_async_route_result_is_returned(run_server):
    server = run_server(
        """
        import asyncio
        from fastrapi import FastrAPI

        app = FastrAPI()


        @app.get("/health")
        def health():
            return {{"status": "ok"}}


        @app.get("/async")
        async def async_route():
            await asyncio.sleep(0)
            return {{"async": True}}


        app.serve(host="127.0.0.1", port={port})
        """
    )

    response = httpx.get(f"{server}/async")
    assert response.status_code == 200
    assert response.json() == {"async": True}


def test_openapi_and_docs_are_served(run_server):
    server = run_server(
        """
        from pydantic import BaseModel
        from fastrapi import FastrAPI


        class Payload(BaseModel):
            name: str
            count: int


        app = FastrAPI(title="Schema API", version="2.0.0", description="docs")


        @app.get("/health")
        def health():
            return {{"status": "ok"}}


        @app.post("/payload")
        def payload(payload: Payload):
            return payload.model_dump()


        app.serve(host="127.0.0.1", port={port})
        """
    )

    response = httpx.get(f"{server}/api-docs/openapi.json")
    assert response.status_code == 200
    spec = response.json()
    assert spec["openapi"] == "3.0.0"
    assert spec["info"]["title"] == "Schema API"
    assert spec["info"]["version"] == "2.0.0"
    assert "/payload" in spec["paths"]
    assert "Payload" in spec["components"]["schemas"]

    docs = httpx.get(f"{server}/docs")
    assert docs.status_code == 200
    assert "swagger" in docs.text.lower()
