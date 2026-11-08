import contextvars

import fastapi
import pytest
from fastapi.testclient import TestClient

import fastrapi
from conftest import LiveServerTestClient

ROUTES = ["/s", "/a", "/s-adep", "/a-adep", "/s-sdep", "/a-sdep", "/a-ydep", "/s-ydep"]
REPEATS = 4


def build(lib, middleware, **kwargs):
    var = contextvars.ContextVar("var", default="unset")
    app = (lib.FastAPI if lib is fastapi else lib.FastrAPI)(**kwargs)
    Depends = lib.Depends

    if middleware == "http":

        @app.middleware("http")
        async def set_var(request, call_next):
            var.set("mw")
            response = await call_next(request)
            response.headers["x-after"] = var.get()
            return response

    elif middleware == "asgi":

        class SetVar:
            def __init__(self, app):
                self.app = app

            async def __call__(self, scope, receive, send):
                if scope["type"] == "http":
                    var.set("mw")
                await self.app(scope, receive, send)

        app.add_middleware(SetVar)

    async def adep():
        var.set("adep")

    def sdep():
        seen = var.get()
        var.set("sdep")
        return seen

    async def aydep():
        var.set("aydep")
        yield

    def sydep():
        var.set("sydep")
        yield

    def mark(tag):
        seen = var.get()
        var.set("leak-" + tag)
        return seen

    @app.get("/s")
    def s():
        return {"seen": mark("s")}

    @app.get("/a")
    async def a():
        return {"seen": mark("a")}

    @app.get("/s-adep", dependencies=[Depends(adep)])
    def s_adep():
        return {"seen": mark("s-adep")}

    @app.get("/a-adep", dependencies=[Depends(adep)])
    async def a_adep():
        return {"seen": mark("a-adep")}

    @app.get("/s-sdep")
    def s_sdep(dep_saw: str = Depends(sdep)):
        return {"seen": mark("s-sdep"), "dep_saw": dep_saw}

    @app.get("/a-sdep")
    async def a_sdep(dep_saw: str = Depends(sdep)):
        return {"seen": mark("a-sdep"), "dep_saw": dep_saw}

    @app.get("/a-ydep", dependencies=[Depends(aydep)])
    async def a_ydep():
        return {"seen": mark("a-ydep")}

    @app.get("/s-ydep", dependencies=[Depends(sydep)])
    def s_ydep():
        return {"seen": mark("s-ydep")}

    return app


def observe(client):
    seen = []
    for route in ROUTES:
        for _ in range(REPEATS):
            response = client.get(route)
            seen.append((route, response.status_code, response.json(), response.headers.get("x-after")))
    return seen


@pytest.mark.parametrize("middleware", ["none", "http", "asgi"])
@pytest.mark.parametrize("sync_to_threadpool", [True, False])
def test_contextvars_match_fastapi(middleware, sync_to_threadpool):
    expected = observe(TestClient(build(fastapi, middleware)))
    with LiveServerTestClient(build(fastrapi, middleware, sync_to_threadpool=sync_to_threadpool)) as client:
        assert observe(client) == expected
