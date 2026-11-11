import inspect
import os
import tempfile

import fastapi
import pytest
from fastapi.testclient import TestClient

import fastrapi


def new_app(lib, **kwargs):
    return lib.FastAPI(**kwargs) if lib is fastapi else lib.FastrAPI(**kwargs)


def submodule(lib, name):
    return __import__(f"{lib.__name__}.{name}", fromlist=["_"])


def client(app):
    return TestClient(app, raise_server_exceptions=False, follow_redirects=False)


def both(run):
    assert run(fastrapi) == run(fastapi)


def test_gzip_matches_fastapi():
    def run(lib):
        gzip = submodule(lib, "middleware.gzip").GZipMiddleware
        app = new_app(lib)
        app.add_middleware(gzip, minimum_size=10)

        @app.get("/large")
        def large():
            return {"data": "x" * 500}

        @app.get("/small")
        def small():
            return {}

        @app.get("/image")
        def image():
            return lib.responses.Response(b"x" * 500, media_type="image/png")

        c = client(app)
        return list(inspect.signature(gzip).parameters), [
            (r.status_code, r.headers.get("content-encoding"), r.headers.get("vary"), r.content)
            for r in (
                c.get("/large", headers={"accept-encoding": "gzip"}),
                c.get("/large", headers={"accept-encoding": "identity"}),
                c.get("/small", headers={"accept-encoding": "gzip"}),
                c.get("/image", headers={"accept-encoding": "gzip"}),
            )
        ]

    both(run)


@pytest.mark.parametrize(
    ("origin", "method", "headers"),
    [
        ("https://a.com", "GET", "x-h"),
        ("https://b.com", "GET", "x-h"),
        ("https://a.com", "DELETE", "x-h"),
        ("https://a.com", "GET", "x-other"),
    ],
)
def test_cors_preflight_matches_fastapi(origin, method, headers):
    def run(lib):
        app = new_app(lib)
        app.add_middleware(
            submodule(lib, "middleware.cors").CORSMiddleware,
            allow_origins=["https://a.com"],
            allow_methods=["GET"],
            allow_headers=["x-h"],
        )

        @app.get("/c")
        def c():
            return {}

        r = client(app).options(
            "/c",
            headers={"origin": origin, "access-control-request-method": method, "access-control-request-headers": headers},
        )
        cors = {k: v for k, v in r.headers.items() if k.startswith("access-control") or k in ("vary", "allow")}
        return r.status_code, r.text, cors

    both(run)


@pytest.mark.parametrize("origins", [["https://a.com"], ["*"]])
def test_cors_simple_matches_fastapi(origins):
    def run(lib):
        app = new_app(lib)
        app.add_middleware(submodule(lib, "middleware.cors").CORSMiddleware, allow_origins=origins, expose_headers=["x-e"])

        @app.get("/c")
        def c():
            return {}

        c = client(app)
        return [
            {k: v for k, v in r.headers.items() if k.startswith("access-control") or k == "vary"}
            for r in (c.get("/c", headers={"origin": "https://a.com"}), c.get("/c"))
        ]

    both(run)


def test_middleware_order_matches_fastapi():
    def run(lib):
        app = new_app(lib)
        order = []

        @app.middleware("http")
        async def first(request, call_next):
            order.append("first")
            return await call_next(request)

        @app.middleware("http")
        async def second(request, call_next):
            order.append("second")
            return await call_next(request)

        @app.get("/")
        def root():
            return {}

        client(app).get("/")
        return order

    both(run)


def test_unhandled_errors_match_fastapi():
    def run(lib):
        app = new_app(lib)

        @app.middleware("http")
        async def broken(request, call_next):
            if request.url.path == "/mw":
                raise ValueError("mw")
            return await call_next(request)

        @app.get("/boom")
        def boom():
            raise ValueError("boom")

        c = client(app)
        responses = [(r.status_code, r.text, r.headers.get("content-type")) for r in (c.get("/mw"), c.get("/boom"))]
        with pytest.raises(ValueError, match="boom"):
            TestClient(app).get("/boom")
        return responses

    both(run)


def test_debug_traceback_matches_fastapi():
    def run(lib):
        app = new_app(lib, debug=True)

        @app.get("/boom")
        def boom():
            raise ValueError("boom")

        r = client(app).get("/boom")
        return r.status_code, r.headers.get("content-type"), "ValueError: boom" in r.text

    both(run)


def test_static_files_match_fastapi():
    directory = tempfile.mkdtemp()
    os.makedirs(os.path.join(directory, "sub"))
    with open(os.path.join(directory, "sub", "index.html"), "w") as f:
        f.write("<p>x</p>")
    with open(os.path.join(directory, "a.txt"), "w") as f:
        f.write("a")

    def run(lib):
        app = new_app(lib)
        app.mount("/st", submodule(lib, "staticfiles").StaticFiles(directory=directory, html=True), name="st")
        c = client(app)
        return [
            (r.status_code, r.headers.get("content-type"), r.headers.get("location"))
            for r in (c.get("/st/sub/"), c.get("/st/a.txt"), c.get("/st/sub"))
        ]

    both(run)


def test_request_client_matches_fastapi():
    def run(lib):
        app = new_app(lib)

        @app.get("/c")
        def c(request: lib.Request):
            host, port = request.client
            return {"host": host, "port": port, "named": request.client.host}

        return client(app).get("/c").json()

    both(run)


def test_routes_argument_matches_fastapi():
    def run(lib):
        def handler():
            return {"ok": 1}

        app = new_app(lib, routes=[submodule(lib, "routing").APIRoute("/r", handler)])
        return client(app).get("/r").json(), sorted(getattr(route, "path", "") for route in app.routes)

    both(run)
