import pytest
from fastapi.testclient import TestClient
from fastrapi import FastrAPI, Depends, Header, Query, Path, Form, File, UploadFile
from fastrapi.responses import JSONResponse
from fastrapi import HTTPException
import asyncio

def test_root_endpoint(client, app):
    @app.get("/")
    def root():
        return {"message": "Hello FastrAPI"}

    response = client.get("/")
    assert response.status_code == 200
    assert response.json() == {"message": "Hello FastrAPI"}


def test_post_without_body(client, app):
    @app.post("/echo")
    def echo():
        return {"status": "ok"}

    response = client.post("/echo")
    assert response.status_code == 200
    assert response.json() == {"status": "ok"}


def test_custom_response_class(client, app):
    @app.get("/custom", response_class=JSONResponse)
    def custom():
        return {"custom": True}

    response = client.get("/custom")
    assert response.status_code == 200
    assert response.json() == {"custom": True}