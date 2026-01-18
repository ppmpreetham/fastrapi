# tests/test_path_params.py
def test_single_path_param(client, app):
    @app.get("/items/{item_id}")
    def read_item(item_id: int):
        return {"item_id": item_id}

    response = client.get("/items/123")
    assert response.status_code == 200
    assert response.json() == {"item_id": 123}

    response = client.get("/items/abc")
    assert response.status_code == 422  # should fail validation if int expected


def test_multiple_path_params(client, app):
    @app.get("/users/{user_id}/items/{item_id}")
    def read_user_item(user_id: str, item_id: int):
        return {"user_id": user_id, "item_id": item_id}

    response = client.get("/users/preetham/items/456")
    assert response.status_code == 200
    assert response.json() == {"user_id": "preetham", "item_id": 456}