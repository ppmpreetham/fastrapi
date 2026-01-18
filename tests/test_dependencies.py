# tests/test_dependencies.py
from fastrapi import Depends


def test_sync_dependency(client, app, sample_dependency):
    @app.get("/user-info")
    def get_user(dep=Depends(sample_dependency)):
        return dep

    response = client.get("/user-info")
    assert response.status_code == 200
    assert response.json() == {"user_id": 42, "role": "admin"}


@pytest.mark.asyncio
async def test_async_dependency(async_client, app, async_sample_dependency):
    @app.get("/async-user")
    async def get_async_user(dep=Depends(async_sample_dependency)):
        return dep

    response = async_client.get("/async-user")
    assert response.status_code == 200
    assert response.json() == {"user_id": 99, "role": "moderator"}