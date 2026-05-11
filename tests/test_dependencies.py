import httpx


def test_dependency_cache_and_subdependencies(run_server):
    server = run_server(
        """
        from fastrapi import Depends, FastrAPI, Query

        app = FastrAPI()
        calls = {{"dep": 0, "sub": 0}}


        @app.get("/health")
        def health():
            return {{"status": "ok"}}


        def sub_dependency(q: int = Query(...)):
            calls["sub"] += 1
            return q * 2


        def dependency(value=Depends(sub_dependency)):
            calls["dep"] += 1
            return {{"value": value, "calls": calls["dep"]}}


        @app.get("/cached")
        def cached(a=Depends(dependency), b=Depends(dependency)):
            return {{"a": a, "b": b, "calls": calls}}


        @app.get("/sub")
        def sub(value=Depends(dependency)):
            return {{"value": value, "calls": calls}}


        @app.get("/uncached")
        def uncached(
            a=Depends(dependency, use_cache=False),
            b=Depends(dependency, use_cache=False),
        ):
            return {{"a": a, "b": b, "calls": calls}}


        app.serve(host="127.0.0.1", port={port})
        """
    )

    response = httpx.get(f"{server}/sub?q=2")
    assert response.status_code == 200
    assert response.json()["value"] == {"value": 4, "calls": 1}

    response = httpx.get(f"{server}/cached?q=3")
    assert response.status_code == 200
    data = response.json()
    assert data["a"] == data["b"]
    assert data["a"]["value"] == 6
    assert data["calls"] == {"dep": 2, "sub": 2}

    response = httpx.get(f"{server}/uncached?q=4")
    assert response.status_code == 200
    data = response.json()
    assert data["a"] != data["b"]
    assert data["a"]["value"] == 8
    assert data["b"]["value"] == 8


def test_dependency_cycle_does_not_recurse_forever(run_server):
    server = run_server(
        """
        import inspect

        from fastrapi import Depends, FastrAPI

        app = FastrAPI()


        def dependency_a(b):
            return {{"a": b}}


        def dependency_b(a):
            return {{"b": a}}


        dependency_a.__signature__ = inspect.Signature(
            [
                inspect.Parameter(
                    "b",
                    inspect.Parameter.POSITIONAL_OR_KEYWORD,
                    default=Depends(dependency_b),
                )
            ]
        )
        dependency_b.__signature__ = inspect.Signature(
            [
                inspect.Parameter(
                    "a",
                    inspect.Parameter.POSITIONAL_OR_KEYWORD,
                    default=Depends(dependency_a),
                )
            ]
        )


        @app.get("/health")
        def health():
            return {{"status": "ok"}}


        @app.get("/cycle")
        def cycle(value=Depends(dependency_a)):
            return value


        app.serve(host="127.0.0.1", port={port})
        """
    )

    response = httpx.get(f"{server}/cycle", timeout=2.0)
    assert response.status_code == 500
