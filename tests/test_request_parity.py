"""Request-time parity with FastAPI 0.162 / Starlette: each test runs one app through both libraries."""

import asyncio
import copy
import pickle
from importlib import import_module
from typing import Annotated

import fastapi
import pytest
from fastapi.testclient import TestClient

import fastrapi

JSON = {"content-type": "application/json"}


def new_app(lib):
    return lib.FastAPI() if lib is fastapi else lib.FastrAPI()


def both(run):
    assert run(fastrapi) == run(fastapi)


def call(lib, register, method, path, **kwargs):
    app = new_app(lib)
    register(app, lib)
    response = TestClient(app, raise_server_exceptions=False).request(method, path, **kwargs)
    return response.status_code, response.text


def parity(register, method, path, **kwargs):
    both(lambda lib: call(lib, register, method, path, **kwargs))


def test_form_keeps_the_case_of_the_multipart_boundary():
    def register(app, lib):
        @app.post("/form")
        async def form(request: lib.Request):
            return dict(await request.form())

    body = b'--aB\r\nContent-Disposition: form-data; name="a"\r\n\r\nv\r\n--aB--\r\n'
    headers = {"content-type": "multipart/form-data; boundary=aB"}
    parity(register, "POST", "/form", content=body, headers=headers)
    assert call(fastrapi, register, "POST", "/form", content=body, headers=headers)[0] == 200


@pytest.mark.parametrize("kind", ["request_form", "file_param"])
def test_multipart_without_boundary_is_a_400(kind):
    def register(app, lib):
        @app.post("/form")
        async def form(request: lib.Request):
            return dict(await request.form())

        @app.post("/file")
        async def file(f: Annotated[bytes, lib.File()]):
            return len(f)

    path = "/form" if kind == "request_form" else "/file"
    parity(register, "POST", path, content=b"x", headers={"content-type": "multipart/form-data"})
    status, text = call(fastrapi, register, "POST", path, content=b"x", headers={"content-type": "multipart/form-data"})
    assert (status, text) == (400, '{"detail":"Missing boundary in multipart."}')


@pytest.mark.parametrize(
    "body",
    [b'\xef\xbb\xbf{"a":1,"b":"x"}', b'{"a":1e999,"b":"x"}', b'{"a":1,"b":"\\ud800"}'],
    ids=["bom", "overflowing-float", "lone-surrogate"],
)
def test_multi_param_json_accepted_like_json_loads(body):
    def register(app, lib):
        @app.post("/multi")
        def multi(a: Annotated[float, lib.Body()], b: Annotated[str, lib.Body()]):
            return f"{a}|{len(b)}"

    parity(register, "POST", "/multi", content=body, headers=JSON)


def test_single_body_with_bom_is_accepted():
    def register(app, lib):
        @app.post("/one")
        def one(a: Annotated[int, lib.Body(embed=True)]):
            return a

    parity(register, "POST", "/one", content=b'\xef\xbb\xbf{"a":1}', headers=JSON)


def test_header_bytes_above_ascii_decode_as_latin_1():
    def register(app, lib):
        @app.get("/h")
        def h(x: Annotated[str, lib.Header()]):
            return x

    parity(register, "GET", "/h", headers={"x": b"caf\xe9"})


@pytest.mark.parametrize("field", ["one", "many"])
def test_upload_file_given_text_is_a_value_error(field):
    def register(app, lib):
        @app.post("/one")
        def one(f: lib.UploadFile):
            return f.filename

        @app.post("/many")
        def many(f: Annotated[list[lib.UploadFile], lib.File()]):
            return len(f)

    parity(register, "POST", f"/{field}", data={"f": "text"})


def test_stream_of_a_bodyless_request():
    def register(app, lib):
        @app.get("/s")
        async def s(request: lib.Request):
            return [chunk.decode() async for chunk in request.stream()]

    parity(register, "GET", "/s")


def test_client_scope_as_a_list():
    def run(lib):
        return repr(lib.Request({"type": "http", "client": ["1.2.3.4", 5], "headers": []}).client)

    both(run)


@pytest.mark.parametrize("scope", [{"type": "bogus"}, {}], ids=["unknown", "missing"])
def test_unknown_or_missing_scope_type(scope):
    async def noop(*_):
        return {}

    def run(lib):
        try:
            asyncio.run(new_app(lib)(dict(scope), noop, noop))
        except BaseException as exc:
            return type(exc).__name__, str(exc)

    both(run)


def test_validation_errors_follow_fastapis_source_order():
    def register(app, lib):
        @app.post("/o/{pid}")
        def o(
            pid: int,
            q: int,
            x: Annotated[int, lib.Body()],
            h: Annotated[int, lib.Header()],
            c: Annotated[int, lib.Cookie()],
        ):
            return 1

    parity(register, "POST", "/o/x?q=y", json="s", headers={"h": "z"}, cookies={"c": "w"})
    parity(register, "POST", "/o/1")


def test_generator_teardown_exception_types():
    def run(lib):
        app = new_app(lib)

        def late():
            yield 1
            raise lib.HTTPException(418, "late")

        def swallow():
            try:
                yield 1
            except Exception:
                pass

        @app.get("/late")
        def a(x: Annotated[int, lib.Depends(late)]):
            return "ok"

        @app.get("/swallow")
        def b(x: Annotated[int, lib.Depends(swallow)]):
            raise ValueError("boom")

        client = TestClient(app)
        out = []
        for path in ("/late", "/swallow"):
            with pytest.raises(RuntimeError) as caught:
                client.get(path)
            out.append((type(caught.value).__name__, str(caught.value)))
        return out

    assert run(fastrapi) == run(fastapi)


def test_validation_exception_args_repr_pickle_and_copy():
    def run(lib):
        error_type = import_module(f"{lib.__name__}.exceptions").RequestValidationError
        exc = error_type([{"a": 1}], body="b")
        clones = [pickle.loads(pickle.dumps(exc)), copy.copy(exc)]
        return repr(exc), exc.args, [(c.errors(), c.body) for c in clones]

    both(run)


def test_dependency_override_receives_security_scopes():
    def run(lib):
        app = new_app(lib)

        def real():
            return "real"

        def override(scopes: lib.security.SecurityScopes):
            return list(scopes.scopes)

        @app.get("/s")
        def s(v: Annotated[object, lib.Security(real, scopes=["s1", "s2"])]):
            return v

        app.dependency_overrides[real] = override
        return TestClient(app).get("/s").json()

    both(run)
