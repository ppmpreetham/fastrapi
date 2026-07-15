import pytest
from fastapi.testclient import TestClient
from fastrapi import FastrAPI
from fastrapi.responses import JSONResponse
import asyncio

@pytest.fixture
def app():
    return FastrAPI(debug=True)


import httpx
import threading
import time
import socket

def get_free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]

class LiveServerTestClient(httpx.Client):
    def __init__(self, app):
        self.app = app
        self.port = get_free_port()
        self.server_thread = None
        super().__init__(base_url=f"http://127.0.0.1:{self.port}")
        
    def _ensure_server_running(self):
        if self.server_thread is None:
            self.server_thread = threading.Thread(
                target=self.app.serve,
                args=("127.0.0.1", self.port),
                daemon=True
            )
            self.server_thread.start()
            time.sleep(0.5)  # Wait for server to start

    def request(self, method, url, **kwargs):
        self._ensure_server_running()
        return super().request(method, url, **kwargs)

@pytest.fixture
def client(app):
    with LiveServerTestClient(app) as c:
        yield c


@pytest.fixture
def async_client(app):
    """For testing async endpoints"""
    with LiveServerTestClient(app) as c:
        yield c


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