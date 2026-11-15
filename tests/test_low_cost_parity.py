import asyncio
import threading
import time

import fastapi
from fastapi.testclient import TestClient
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
