import asyncio
import threading
import time
from typing import Annotated, Literal

import fastapi
import pytest
from fastapi.testclient import TestClient
from pydantic import BaseModel
from starlette.background import BackgroundTask

import fastrapi


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def both(run):
    assert run(fastrapi) == run(fastapi)


def test_parameter_bounds_keep_python_number_types():
    def run(lib):
        app = new_app(lib)

        @app.get("/bounds")
        def bounds(a: int = lib.Query(gt=3, le=10), b: float = lib.Query(ge=0.0, lt=7)):
            return a

        parameters = app.openapi()["paths"]["/bounds"]["get"]["parameters"]
        return [{key: (value, type(value).__name__) for key, value in p["schema"].items() if "imum" in key} for p in parameters]

    both(run)


def test_background_tasks_run_in_order_and_stop_at_first_failure():
    def run(lib):
        log = []
        app = new_app(lib)

        async def slow(tag):
            await asyncio.sleep(0.02)
            log.append(tag)

        def sync(tag):
            log.append((tag, threading.current_thread() is threading.main_thread()))

        def boom():
            raise RuntimeError("boom")

        @app.get("/order")
        def order(tasks: lib.BackgroundTasks):
            tasks.add_task(slow, "a1")
            tasks.add_task(sync, "s1")
            tasks.add_task(slow, "a2")
            tasks.add_task(boom)
            tasks.add_task(sync, "never")
            return "ok"

        @app.get("/response")
        def response():
            return lib.responses.JSONResponse("ok", background=BackgroundTask(slow, "single"))

        @app.get("/call")
        async def call():
            tasks = lib.BackgroundTasks()
            tasks.add_task(slow, "c1")
            tasks.add_task(sync, "c2")
            await tasks()
            return [task.is_async for task in tasks.tasks]

        client = TestClient(app, raise_server_exceptions=False)
        bodies = [client.get(path).json() for path in ("/order", "/response", "/call")]
        time.sleep(0.2)
        return bodies, log

    both(run)


def test_get_websocket_app_solves_dependencies():
    from starlette.applications import Starlette
    from starlette.routing import WebSocketRoute
    from starlette.testclient import TestClient as StarletteClient
    from starlette.websockets import WebSocketDisconnect

    from fastrapi.dependencies.utils import get_dependant
    from fastrapi.routing import get_websocket_app

    def prefix():
        return "hi"

    async def endpoint(websocket: fastrapi.WebSocket, name: str, greeting: str = fastrapi.Depends(prefix)):
        await websocket.accept()
        await websocket.send_text(f"{greeting} {name}")
        await websocket.close()

    app = get_websocket_app(get_dependant(path="/ws", call=endpoint))
    client = StarletteClient(Starlette(routes=[WebSocketRoute("/ws", app)]))
    with client.websocket_connect("/ws?name=bob") as websocket:
        assert websocket.receive_text() == "hi bob"
    try:
        with client.websocket_connect("/ws") as websocket:
            websocket.receive_text()
    except Exception as exc:
        assert isinstance(exc, (WebSocketDisconnect, fastrapi.exceptions.WebSocketRequestValidationError))
    else:
        raise AssertionError("missing query parameter was accepted")


def test_route_dependencies_keep_marker_options():
    def run(lib):
        events = []

        def function_session():
            yield
            events.append("function cleanup")

        def request_session():
            yield
            events.append("request cleanup")

        def counted():
            events.append("counted")

        def scoped(security_scopes: lib.security.SecurityScopes):
            events.append(security_scopes.scopes)

        router = lib.APIRouter(dependencies=[lib.Depends(function_session, scope="function")])

        @router.get(
            "/stream",
            dependencies=[
                lib.Depends(request_session),
                lib.Depends(counted, use_cache=False),
                lib.Security(scoped, scopes=["items"]),
            ],
        )
        def stream(again=lib.Depends(counted), fresh=lib.Depends(counted, use_cache=False)):
            def chunks():
                yield "chunk"

            return lib.responses.StreamingResponse(chunks())

        app = new_app(lib)
        app.include_router(router)
        body = TestClient(app).get("/stream").text
        return body, events

    both(run)


class Cat(BaseModel):
    kind: Literal["cat"]
    meow: str


class Dog(BaseModel):
    kind: Literal["dog"]
    bark: str
    tags: list[str] = []


class Profile(BaseModel):
    name: str
    age: int = 1


class Extra(BaseModel):
    note: str


def form_app(lib):
    app = new_app(lib)

    @app.post("/union")
    def union(pet: Annotated[Cat | Dog, lib.Form()]):
        return pet

    @app.post("/optional-union")
    def optional_union(pet: Annotated[Cat | Dog | None, lib.Form()] = None):
        return pet

    @app.post("/optional")
    def optional(profile: Annotated[Profile | None, lib.Form()] = None):
        return profile

    @app.post("/combined")
    def combined(profile: Annotated[Profile, lib.Form()], extra: Annotated[Extra, lib.Form()]):
        return [profile, extra]

    @app.post("/mixed")
    def mixed(profile: Annotated[Profile, lib.Form()], note: Annotated[str, lib.Form()] = "x"):
        return [profile, note]

    @app.post("/json")
    def json_body(count: int = lib.Body(5), profile: Profile = lib.Body(Profile(name="p")), label: str = lib.Body(embed=True)):
        return count

    return app


@pytest.mark.parametrize(
    ("path", "data"),
    [
        ("/union", {"kind": "dog", "bark": "w", "tags": ["a", "b"]}),
        ("/union", {"kind": "cat", "meow": "m", "bark": ""}),
        ("/union", {"kind": "x"}),
        ("/optional-union", {}),
        ("/optional-union", {"kind": "cat", "meow": "m"}),
        ("/optional-union", {"other": "1"}),
    ],
)
def test_form_model_unions(path, data):
    def run(lib):
        response = TestClient(form_app(lib)).post(path, data=data)
        return response.status_code, response.json()

    both(run)


def test_form_model_request_body_schemas():
    def run(lib):
        spec = form_app(lib).openapi()
        bodies = {path: item["post"]["requestBody"] for path, item in spec["paths"].items()}
        return bodies, spec["components"]["schemas"]

    both(run)
