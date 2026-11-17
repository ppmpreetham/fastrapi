import pytest

from fastrapi import Depends
from fastrapi.security import OAuth2PasswordRequestForm, OAuth2PasswordRequestFormStrict


def test_oauth2_password_request_form(client, app):
    @app.post("/token")
    async def token(form_data=Depends(OAuth2PasswordRequestForm)):
        return {
            "username": form_data.username,
            "password": form_data.password,
            "scope": form_data.scopes,
        }

    response = client.post(
        "/token",
        data={"username": "alice", "password": "secret", "scope": "read write"},
    )
    assert response.status_code == 200
    assert response.json() == {
        "username": "alice",
        "password": "secret",
        "scope": ["read", "write"],
    }


def test_oauth2_password_request_form_strict():
    with pytest.raises(TypeError):
        OAuth2PasswordRequestFormStrict(username="alice", password="secret")
