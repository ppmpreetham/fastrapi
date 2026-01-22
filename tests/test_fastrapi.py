"""
FastrAPI Comprehensive Test Suite
Run with: pytest test_fastrapi.py -v
"""

import pytest
import asyncio
import httpx
from pydantic import BaseModel
from typing import Optional, List
from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse, HTMLResponse, PlainTextResponse, RedirectResponse
from fastrapi import HTTPException
from fastrapi import status
from fastrapi import Query, Path, Body, Header, Cookie, Depends, Security
from fastrapi import SecurityScopes
from threading import Thread
import time


# ============================================================================
# Pydantic Models for Testing
# ============================================================================

class User(BaseModel):
    name: str
    age: int
    email: Optional[str] = None


class Address(BaseModel):
    street: str
    city: str
    zip: str


class Item(BaseModel):
    id: int
    name: str
    price: float
    tags: List[str] = []


class LoginCredentials(BaseModel):
    username: str
    password: str


# ============================================================================
# Test Fixtures
# ============================================================================

@pytest.fixture(scope="module")
def app():
    """Create a test FastrAPI application"""
    app = FastrAPI(
        title="Test API",
        version="1.0.0",
        description="FastrAPI Test Suite"
    )
    return app


@pytest.fixture(scope="module")
def client(app):
    """Create HTTP client and start server in background"""
    
    # Register all test routes
    register_test_routes(app)
    
    # Start server in background thread
    def run_server():
        app.serve(host="127.0.0.1", port=8888)
    
    server_thread = Thread(target=run_server, daemon=True)
    server_thread.start()
    
    # Wait for server to start
    time.sleep(2)
    
    # Create client
    client = httpx.Client(base_url="http://127.0.0.1:8888")
    
    yield client
    
    client.close()


@pytest.fixture(scope="module")
async def async_client(app):
    """Create async HTTP client"""
    register_test_routes(app)
    
    def run_server():
        app.serve(host="127.0.0.1", port=8889)
    
    server_thread = Thread(target=run_server, daemon=True)
    server_thread.start()
    time.sleep(2)
    
    async with httpx.AsyncClient(base_url="http://127.0.0.1:8889") as client:
        yield client


# ============================================================================
# Route Registration Helper
# ============================================================================

def register_test_routes(app: FastrAPI):
    """Register all test routes"""
    
    # ========== Basic Routes ==========
    
    @app.get("/")
    def root():
        return {"message": "Hello World"}
    
    @app.get("/items/{item_id}")
    def get_item(item_id: int):
        return {"item_id": item_id, "type": type(item_id).__name__}
    
    @app.get("/search")
    def search(q: str, limit: int = 10):
        return {"query": q, "limit": limit}
    
    # ========== Pydantic Validation ==========
    
    @app.post("/users")
    def create_user(user: User):
        return {"created": user.model_dump(), "name_upper": user.name.upper()}
    
    @app.post("/register")
    def register(user: User, address: Address):
        return {
            "user": user.model_dump(),
            "address": address.model_dump(),
            "full_location": f"{address.city}, {address.zip}"
        }
    
    @app.put("/items/{item_id}")
    def update_item(item_id: int, item: Item):
        return {"item_id": item_id, "item": item.model_dump()}
    
    # ========== Response Types ==========
    
    @app.get("/html")
    def get_html() -> HTMLResponse:
        return HTMLResponse("<h1>Hello HTML</h1>", status_code=200)
    
    @app.get("/json")
    def get_json()-> JSONResponse:
        return JSONResponse({"format": "json"}, status_code=200)
    
    @app.get("/text") 
    def get_text()-> PlainTextResponse:
        return PlainTextResponse("Plain text response", status_code=200)
    
    @app.get("/redirect") 
    def get_redirect()-> RedirectResponse:
        return RedirectResponse("/", status_code=307)
    
    # ========== Exception Handling ==========
    
    @app.get("/error/{code}")
    def trigger_error(code: int):
        if code == 404:
            raise HTTPException(status_code=404, detail="Item not found")
        elif code == 400:
            raise HTTPException(status_code=400, detail="Bad request")
        elif code == 500:
            raise HTTPException(status_code=500, detail="Internal server error")
        return {"code": code}
    
    # ========== Query Parameters with Validation ==========
    
    @app.get("/validate/query")
    def validate_query(
        age: int = Query(default=18, ge=0, le=150),
        name: str = Query(default="Anonymous", min_length=1, max_length=50)
    ):
        return {"age": age, "name": name}
    
    # ========== Path Parameters with Validation ==========
    
    @app.get("/validate/path/{user_id}")
    def validate_path(
        user_id: int = Path(..., ge=1, le=1000)
    ):
        return {"user_id": user_id}
    
    # ========== Multiple Parameter Types ==========
    
    @app.post("/complex/{path_param}")
    def complex_endpoint(
        path_param: str,
        query_param: str,
        item: Item
    ):
        return {
            "path": path_param,
            "query": query_param,
            "body": item.model_dump()
        }
    
    # ========== Dependencies ==========
    
    def get_current_user():
        return {"user_id": 123, "username": "testuser"}
    
    @app.get("/protected")
    def protected_route(user = Depends(get_current_user)):
        return {"message": "Protected data", "user": user}
    
    def verify_token(token: str = Header(default=None)):
        if not token or token != "valid-token":
            raise HTTPException(status_code=401, detail="Invalid token")
        return token
    
    @app.get("/auth")
    def authenticated(token = Depends(verify_token)):
        return {"message": "Authenticated", "token": token}
    
    # ========== Security Scopes ==========
    
    def verify_scopes(
        security_scopes: SecurityScopes,
        token: str = Header(default=None)
    ):
        if not token:
            raise HTTPException(status_code=401, detail="No token")
        return {"scopes": security_scopes.scopes, "token": token}
    
    @app.get("/admin")
    def admin_only(auth = Security(verify_scopes, scopes=["admin"])):
        return {"message": "Admin access", "auth": auth}
    
    # ========== Async Routes ==========
    
    @app.get("/async/data")
    async def async_data():
        await asyncio.sleep(0.1)
        return {"async": True, "data": "processed"}
    
    # ========== Status Codes ==========
    
    @app.post("/created")
    def create_resource():
        return JSONResponse({"id": 1}, status_code=status.HTTP_201_CREATED)
    
    @app.delete("/items/{item_id}")
    def delete_item(item_id: int):
        return JSONResponse({"deleted": item_id}, status_code=status.HTTP_200_OK)
    
    # ========== Empty Response ==========
    
    @app.get("/empty")
    def empty_response():
        return None


# ============================================================================
# Test Cases
# ============================================================================

class TestBasicRoutes:
    """Test basic routing functionality"""
    
    def test_root_endpoint(self, client):
        response = client.get("/")
        assert response.status_code == 200
        assert response.json() == {"message": "Hello World"}
    
    def test_path_parameters(self, client):
        response = client.get("/items/42")
        assert response.status_code == 200
        data = response.json()
        assert data["item_id"] == 42
        assert data["type"] == "int"
    
    def test_query_parameters(self, client):
        response = client.get("/search?q=fastapi&limit=20")
        assert response.status_code == 200
        data = response.json()
        assert data["query"] == "fastapi"
        assert data["limit"] == 20
    
    def test_query_parameters_default(self, client):
        response = client.get("/search?q=test")
        assert response.status_code == 200
        assert response.json()["limit"] == 10


class TestPydanticValidation:
    """Test Pydantic model validation"""
    
    def test_single_model_valid(self, client):
        user_data = {"name": "John Doe", "age": 30, "email": "john@example.com"}
        response = client.post("/users", json=user_data)
        assert response.status_code == 200
        data = response.json()
        assert data["created"]["name"] == "John Doe"
        assert data["name_upper"] == "JOHN DOE"
    
    def test_single_model_invalid(self, client):
        invalid_data = {"name": "John"}  # Missing required 'age'
        response = client.post("/users", json=invalid_data)
        assert response.status_code == 422
    
    def test_multiple_models_valid(self, client):
        payload = {
            "user": {"name": "Alice", "age": 25},
            "address": {"street": "123 Main St", "city": "NYC", "zip": "10001"}
        }
        response = client.post("/register", json=payload)
        assert response.status_code == 200
        data = response.json()
        assert data["user"]["name"] == "Alice"
        assert data["address"]["city"] == "NYC"
        assert data["full_location"] == "NYC, 10001"
    
    def test_multiple_models_missing_field(self, client):
        payload = {
            "user": {"name": "Bob", "age": 30},
            "address": {"street": "456 Oak Ave", "city": "LA"}  # Missing 'zip'
        }
        response = client.post("/register", json=payload)
        assert response.status_code == 422
    
    def test_path_and_body_combination(self, client):
        item_data = {
            "id": 1,
            "name": "Widget",
            "price": 19.99,
            "tags": ["new", "featured"]
        }
        response = client.put("/items/123", json=item_data)
        assert response.status_code == 200
        data = response.json()
        assert data["item_id"] == 123
        assert data["item"]["name"] == "Widget"


class TestResponseTypes:
    """Test different response types"""
    
    def test_html_response(self, client):
        response = client.get("/html")
        assert response.status_code == 200
        assert "text/html" in response.headers.get("content-type", "")
        assert "<h1>Hello HTML</h1>" in response.text
    
    def test_json_response(self, client):
        response = client.get("/json")
        assert response.status_code == 200
        assert response.json()["format"] == "json"
    
    def test_text_response(self, client):
        response = client.get("/text")
        assert response.status_code == 200
        assert "text/plain" in response.headers.get("content-type", "")
        assert response.text == "Plain text response"
    
    def test_redirect_response(self, client):
        response = client.get("/redirect", follow_redirects=False)
        assert response.status_code == 307
        assert response.headers["location"] == "/"


class TestExceptionHandling:
    """Test HTTP exception handling"""
    
    def test_404_exception(self, client):
        response = client.get("/error/404")
        assert response.status_code == 404
        assert response.json()["detail"] == "Item not found"
    
    def test_400_exception(self, client):
        response = client.get("/error/400")
        assert response.status_code == 400
        assert response.json()["detail"] == "Bad request"
    
    def test_500_exception(self, client):
        response = client.get("/error/500")
        assert response.status_code == 500
        assert response.json()["detail"] == "Internal server error"
    
    def test_no_exception(self, client):
        response = client.get("/error/200")
        assert response.status_code == 200
        assert response.json()["code"] == 200


class TestParameterValidation:
    """Test parameter validation with constraints"""
    
    def test_query_validation_valid(self, client):
        response = client.get("/validate/query?age=25&name=John")
        assert response.status_code == 200
        data = response.json()
        assert data["age"] == 25
        assert data["name"] == "John"
    
    def test_query_validation_defaults(self, client):
        response = client.get("/validate/query")
        assert response.status_code == 200
        data = response.json()
        assert data["age"] == 18
        assert data["name"] == "Anonymous"
    
    def test_path_validation_valid(self, client):
        response = client.get("/validate/path/500")
        assert response.status_code == 200
        assert response.json()["user_id"] == 500
    
    def test_complex_parameters(self, client):
        item_data = {
            "id": 1,
            "name": "Test Item",
            "price": 9.99
        }
        response = client.post(
            "/complex/test-path?query_param=test-query",
            json=item_data
        )
        assert response.status_code == 200
        data = response.json()
        assert data["path"] == "test-path"
        assert data["query"] == "test-query"
        assert data["body"]["name"] == "Test Item"


class TestDependencies:
    """Test dependency injection"""
    
    def test_simple_dependency(self, client):
        response = client.get("/protected")
        assert response.status_code == 200
        data = response.json()
        assert data["user"]["username"] == "testuser"
        assert data["message"] == "Protected data"
    
    def test_header_dependency_valid(self, client):
        response = client.get("/auth", headers={"token": "valid-token"})
        assert response.status_code == 200
        data = response.json()
        assert data["message"] == "Authenticated"
        assert data["token"] == "valid-token"
    
    def test_header_dependency_invalid(self, client):
        response = client.get("/auth", headers={"token": "invalid-token"})
        assert response.status_code == 401
    
    def test_header_dependency_missing(self, client):
        response = client.get("/auth")
        assert response.status_code == 401
    
    def test_security_scopes(self, client):
        response = client.get("/admin", headers={"token": "admin-token"})
        assert response.status_code == 200
        data = response.json()
        assert data["message"] == "Admin access"
        assert "admin" in data["auth"]["scopes"]


class TestStatusCodes:
    """Test HTTP status codes"""
    
    def test_created_status(self, client):
        response = client.post("/created")
        assert response.status_code == 201
        assert response.json()["id"] == 1
    
    def test_delete_status(self, client):
        response = client.delete("/items/42")
        assert response.status_code == 200
        assert response.json()["deleted"] == 42
    
    def test_empty_response(self, client):
        response = client.get("/empty")
        assert response.status_code == 204  # No Content


class TestOpenAPISpec:
    """Test OpenAPI specification generation"""
    
    def test_openapi_endpoint(self, client):
        response = client.get("/api-docs/openapi.json")
        assert response.status_code == 200
        spec = response.json()
        
        # Check basic structure
        assert spec["openapi"] == "3.0.0"
        assert spec["info"]["title"] == "Test API"
        assert spec["info"]["version"] == "1.0.0"
        
        # Check paths are registered
        assert "/users" in spec["paths"]
        assert "/register" in spec["paths"]
        assert "/items/{item_id}" in spec["paths"]
        
        # Check components/schemas for Pydantic models
        assert "User" in spec["components"]["schemas"]
        assert "Address" in spec["components"]["schemas"]
        assert "Item" in spec["components"]["schemas"]
        
        # Check schema properties
        user_schema = spec["components"]["schemas"]["User"]
        assert "name" in user_schema["properties"]
        assert "age" in user_schema["properties"]
        assert user_schema["required"] == ["name", "age"]
    
    def test_docs_ui(self, client):
        response = client.get("/docs")
        assert response.status_code == 200
        assert "swagger" in response.text.lower()


class TestEdgeCases:
    """Test edge cases and error conditions"""
    
    def test_nonexistent_route(self, client):
        response = client.get("/nonexistent")
        assert response.status_code == 404
    
    def test_wrong_method(self, client):
        response = client.post("/")  # Root only accepts GET
        assert response.status_code == 405
    
    def test_malformed_json(self, client):
        response = client.post(
            "/users",
            content="not valid json",
            headers={"content-type": "application/json"}
        )
        assert response.status_code in [400, 422]
    
    def test_type_conversion_error(self, client):
        response = client.get("/items/not-a-number")
        assert response.status_code in [400, 422]


# ============================================================================
# Run Tests
# ============================================================================

if __name__ == "__main__":
    pytest.main([__file__, "-v", "--tb=short"])