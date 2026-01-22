import pytest
from fastapi.testclient import TestClient
from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse
# from fastrapi import HTTPException
import asyncio


@pytest.fixture
def app():
    return FastrAPI(debug=True)


@pytest.fixture
def client(app):
    return TestClient(app)


@pytest.fixture
def async_client(app):
    """For testing async endpoints"""
    return TestClient(app)


@pytest.fixture
def sample_dependency():
    def dep():
        return {"user_id": 42, "role": "admin"}

    return dep


@pytest.fixture
def async_sample_dependency():
    async def dep():
        await asyncio.sleep(0.01)  # simulate async work
        return {"user_id": 99, "role": "moderator"}

    return dep