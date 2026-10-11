# tests/test_exceptions.py
from fastrapi import HTTPException


def test_http_exception(client, app):
    @app.get("/error")
    def raise_error():
        raise HTTPException(status_code=418, detail="I'm a teapot")

    response = client.get("/error")
    assert response.status_code == 418
    assert response.json() == {"detail": "I'm a teapot"}


def test_http_exception_default_detail():
    error = HTTPException(status_code=500)
    assert str(error) == "500: Internal Server Error"
    assert str(HTTPException(status_code=404)) == "404: Not Found"
