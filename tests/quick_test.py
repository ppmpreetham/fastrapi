"""
Quick Smoke Test for FastrAPI
Run this after any changes to ensure core functionality works
Usage: python quick_test.py
"""

from pydantic import BaseModel
from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse, HTMLResponse
from fastrapi import HTTPException
from fastrapi.params import Depends
import httpx
import time
from threading import Thread


# Models
class User(BaseModel):
    name: str
    age: int


class Address(BaseModel):
    street: str
    city: str
    zip: str


# Create app
app = FastrAPI(title="Quick Test", version="1.0.0")


# Routes
@app.get("/")
def root():
    return {"status": "ok"}


@app.post("/users")
def create_user(user: User):
    return {"user": user.model_dump()}


@app.post("/register")
def register(user: User, address: Address):
    return {"user": user.model_dump(), "address": address.model_dump()}


@app.get("/items/{item_id}")
def get_item(item_id: int):
    return {"item_id": item_id}


@app.get("/error")
def trigger_error():
    raise HTTPException(status_code=404, detail="Not found")


def get_user():
    return {"user_id": 123}


@app.get("/protected")
def protected(user = Depends(get_user)):
    return {"user": user}


# Test runner
def run_tests():
    """Run quick smoke tests"""
    print("🚀 Starting FastrAPI Quick Test...")
    
    # Start server
    def start_server():
        app.serve(host="127.0.0.1", port=9999)
    
    server = Thread(target=start_server, daemon=True)
    server.start()
    time.sleep(2)
    
    client = httpx.Client(base_url="http://127.0.0.1:9999", timeout=5.0)
    
    tests_passed = 0
    tests_failed = 0
    
    def test(name, func):
        nonlocal tests_passed, tests_failed
        try:
            func()
            print(f"✅ {name}")
            tests_passed += 1
        except AssertionError as e:
            print(f"❌ {name}: {e}")
            tests_failed += 1
        except Exception as e:
            print(f"💥 {name}: {e}")
            tests_failed += 1
    
    # Run tests
    test("Basic GET", lambda: (
        r := client.get("/"),
        assert r.status_code == 200,
        assert r.json()["status"] == "ok"
    )[-1])
    
    test("Path parameters", lambda: (
        r := client.get("/items/42"),
        assert r.status_code == 200,
        assert r.json()["item_id"] == 42
    )[-1])
    
    test("Single Pydantic model", lambda: (
        r := client.post("/users", json={"name": "Alice", "age": 30}),
        assert r.status_code == 200,
        assert r.json()["user"]["name"] == "Alice"
    )[-1])
    
    test("Multiple Pydantic models", lambda: (
        r := client.post("/register", json={
            "user": {"name": "Bob", "age": 25},
            "address": {"street": "123 Main", "city": "NYC", "zip": "10001"}
        }),
        assert r.status_code == 200,
        assert r.json()["user"]["name"] == "Bob",
        assert r.json()["address"]["city"] == "NYC"
    )[-1])
    
    test("Validation error", lambda: (
        r := client.post("/users", json={"name": "Invalid"}),
        assert r.status_code == 422
    )[-1])
    
    test("HTTPException", lambda: (
        r := client.get("/error"),
        assert r.status_code == 404,
        assert r.json()["detail"] == "Not found"
    )[-1])
    
    test("Dependencies", lambda: (
        r := client.get("/protected"),
        assert r.status_code == 200,
        assert r.json()["user"]["user_id"] == 123
    )[-1])
    
    test("OpenAPI spec", lambda: (
        r := client.get("/api-docs/openapi.json"),
        assert r.status_code == 200,
        spec := r.json(),
        assert spec["openapi"] == "3.0.0",
        assert "User" in spec["components"]["schemas"],
        assert "Address" in spec["components"]["schemas"]
    )[-1])
    
    # Summary
    print(f"\n{'='*50}")
    print(f"Tests Passed: {tests_passed}")
    print(f"Tests Failed: {tests_failed}")
    print(f"{'='*50}")
    
    if tests_failed == 0:
        print("🎉 All tests passed!")
        return 0
    else:
        print("⚠️  Some tests failed")
        return 1


if __name__ == "__main__":
    exit(run_tests())