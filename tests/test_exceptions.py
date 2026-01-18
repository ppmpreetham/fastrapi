# tests/test_exceptions.py
from fastrapi.exceptions import HTTPException


def test_http_exception(client, app):
    @app.get("/error")
    def raise_error():
        raise HTTPException(status_code=418, detail="I'm a teapot")

    response = client.get("/error")
    assert response.status_code == 418
    assert response.json() == {"detail": "I'm a teapot"}