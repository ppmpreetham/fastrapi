import pytest

from fastrapi import Depends
from fastrapi.security import OAuth2PasswordRequestForm, OAuth2PasswordRequestFormStrict


def test_oauth2_password_request_form(client, app):
    form = OAuth2PasswordRequestForm()

    @app.post("/token")
    async def token(form_data=Depends(form)):
        return {
            "username": form_data.username,
            "password": form_data.password,
            "scope": form_data.scope,
        }

    response = client.post(
        "/token",
        data={"username": "alice", "password": "secret", "scope": "read write"},
    )
    assert response.status_code == 200
    assert response.json() == {
        "username": "alice",
        "password": "secret",
        "scope": "read write",
    }


def test_oauth2_password_request_form_strict():
    with pytest.raises(ValueError):
        OAuth2PasswordRequestFormStrict(grant_type="client_credentials")
