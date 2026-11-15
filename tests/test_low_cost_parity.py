import fastapi
from fastapi.testclient import TestClient

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
