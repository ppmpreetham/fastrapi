# tests/test_body_validation.py
from pydantic import BaseModel


class Item(BaseModel):
    name: str
    price: float


def test_single_pydantic_body(client, app):
    @app.post("/items/")
    def create_item(item: Item):
        return item.dict()

    response = client.post(
        "/items/",
        json={"name": "Laptop", "price": 999.99}
    )
    assert response.status_code == 200
    assert response.json()["name"] == "Laptop"


def test_validation_error(client, app):
    @app.post("/users/")
    def create_user(user: Item):
        return user

    response = client.post(
        "/users/",
        json={"name": "test", "price": "invalid"}  # price should be float
    )
    assert response.status_code == 422
    assert "detail" in response.json()