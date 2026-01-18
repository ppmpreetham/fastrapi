# tests/test_query_params.py
from fastrapi import Query


def test_query_param_optional(client, app):
    @app.get("/search")
    def search(q: str | None = Query(None)):
        return {"query": q}

    response = client.get("/search?q=python")
    assert response.json() == {"query": "python"}

    response = client.get("/search")
    assert response.json() == {"query": None}


def test_query_param_required(client, app):
    @app.get("/items/")
    def read_items(q: str = Query(...)):
        return {"q": q}

    response = client.get("/items/?q=fastapi")
    assert response.status_code == 200

    response = client.get("/items/")
    assert response.status_code == 422  # missing required query