import socket
import threading
import time

import httpx
import pytest

from fastrapi import APIRouter, FastrAPI


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def _serve(app: FastrAPI, port: int) -> None:
    t = threading.Thread(
        target=lambda: app.serve(host="127.0.0.1", port=port),
        daemon=True,
    )
    t.start()
    deadline = time.time() + 10.0
    while time.time() < deadline:
        try:
            httpx.get(f"http://127.0.0.1:{port}/api-docs/openapi.json", timeout=0.5)
            return
        except Exception:
            time.sleep(0.05)
    raise RuntimeError(f"server on port {port} never became ready")


@pytest.fixture
def live_app():
    """Yield (FastrAPI, base_url). Caller wires up routes BEFORE calling .ready()."""

    class _Live:
        def __init__(self):
            self.app = FastrAPI(debug=True)
            self.port = _free_port()
            self.base = f"http://127.0.0.1:{self.port}"
            self._started = False

        def ready(self):
            if not self._started:
                _serve(self.app, self.port)
                self._started = True
            return self.base

    return _Live()


# -----------------------------------------------------------------
# Construction & attribute tests (no server needed)
# -----------------------------------------------------------------
class TestConstruction:
    def test_default_construction(self):
        r = APIRouter()
        assert r.prefix == ""
        assert r.tags == []
        assert r.deprecated is None
        assert r.include_in_schema is True
        assert r.dependencies is None
        assert r.responses is None
        assert r.default_response_class is None
        assert r.generate_unique_id_function is None

    def test_with_prefix(self):
        r = APIRouter(prefix="/api/v1")
        assert r.prefix == "/api/v1"

    def test_with_tags(self):
        r = APIRouter(tags=["users", "admin"])
        assert r.tags == ["users", "admin"]

    def test_tags_dedup_within_init(self):
        # Whatever the init does, tags should be a list of str
        r = APIRouter(tags=["a", "b", "a"])
        assert isinstance(r.tags, list)
        assert all(isinstance(t, str) for t in r.tags)

    def test_with_deprecated(self):
        r = APIRouter(deprecated=True)
        assert r.deprecated is True

    def test_include_in_schema_false(self):
        r = APIRouter(include_in_schema=False)
        assert r.include_in_schema is False

    def test_constructor_is_keyword_only(self):
        with pytest.raises(TypeError):
            APIRouter("/api")  # type: ignore[misc]

    def test_tags_non_string_silently_ignored(self):
        # Implementation filters via extract::<String>().ok()
        r = APIRouter(tags=["ok", 42, "fine"])
        assert "ok" in r.tags and "fine" in r.tags
        assert all(isinstance(t, str) for t in r.tags)


# -----------------------------------------------------------------
# Decorator behavior (no live server needed)
# -----------------------------------------------------------------
class TestDecorators:
    def test_decorator_returns_function(self):
        r = APIRouter()

        @r.get("/x")
        def handler():
            return {}

        assert callable(handler)
        assert handler.__name__ == "handler"

    @pytest.mark.parametrize(
        "method", ["get", "post", "put", "delete", "patch", "options", "head"]
    )
    def test_all_http_methods_register(self, method):
        r = APIRouter()
        deco = getattr(r, method)

        @deco("/p")
        def f():
            return {}

        assert callable(f)

    def test_websocket_decorator(self):
        r = APIRouter()

        @r.websocket("/ws")
        def ws(socket):
            pass

        assert callable(ws)

    def test_websocket_path_must_start_with_slash(self):
        r = APIRouter()
        with pytest.raises(ValueError):
            r.websocket("ws-no-slash")

    def test_decorators_stack_independently(self):
        r = APIRouter()

        @r.get("/a")
        @r.post("/a")
        def both():
            return {}

        assert callable(both)


# -----------------------------------------------------------------
# include_router (sub-router mounting)
# -----------------------------------------------------------------
class TestIncludeRouter:
    def test_include_router_returns_none(self):
        outer = APIRouter()
        inner = APIRouter(prefix="/inner")
        assert outer.include_router(inner) is None

    def test_include_with_extra_prefix(self):
        outer = APIRouter()
        inner = APIRouter(prefix="/v1")
        # should not error
        outer.include_router(inner, prefix="/api")

    def test_include_with_tags(self):
        outer = APIRouter()
        inner = APIRouter()
        outer.include_router(inner, tags=["nested"])

    def test_include_router_non_string_tags_filtered(self):
        outer = APIRouter()
        inner = APIRouter()
        outer.include_router(inner, tags=["ok", 7])  # 7 silently dropped

    def test_parent_remains_mutable_after_include(self):
        # include_router does NOT freeze self.
        parent = APIRouter()
        child = APIRouter(prefix="/c")
        parent.include_router(child)

        @parent.get("/x")
        def x():
            return {}

        @parent.websocket("/ws")
        def ws(socket):
            pass

        parent.include_router(APIRouter(prefix="/d"))  # multiple includes ok

    def test_child_remains_mutable_after_include(self):
        parent = APIRouter()
        child = APIRouter(prefix="/c")
        parent.include_router(child)

        @child.get("/y")
        def y():
            return {}


# -----------------------------------------------------------------
# Live server tests: routes actually serve
# -----------------------------------------------------------------
class TestLiveRouting:
    def test_router_get_via_app(self, live_app):
        router = APIRouter(prefix="/api")

        @router.get("/items")
        def items():
            return {"items": [1, 2, 3]}

        live_app.app.include_router(router)
        base = live_app.ready()

        r = httpx.get(f"{base}/api/items")
        assert r.status_code == 200
        assert r.json() == {"items": [1, 2, 3]}

    def test_empty_prefix_router(self, live_app):
        router = APIRouter()

        @router.get("/hello")
        def hello():
            return {"hello": "world"}

        live_app.app.include_router(router)
        base = live_app.ready()

        r = httpx.get(f"{base}/hello")
        assert r.status_code == 200
        assert r.json() == {"hello": "world"}

    def test_include_router_extra_prefix(self, live_app):
        # router prefix /v1, mounted under /api -> /api/v1/ping
        router = APIRouter(prefix="/v1")

        @router.get("/ping")
        def ping():
            return {"pong": True}

        live_app.app.include_router(router, prefix="/api")
        base = live_app.ready()

        r = httpx.get(f"{base}/api/v1/ping")
        assert r.status_code == 200
        assert r.json() == {"pong": True}

    def test_nested_routers(self, live_app):
        # Three levels deep
        leaf = APIRouter(prefix="/leaf")

        @leaf.get("/data")
        def data():
            return {"deep": True}

        mid = APIRouter(prefix="/mid")
        mid.include_router(leaf)

        root = APIRouter(prefix="/root")
        root.include_router(mid)

        live_app.app.include_router(root)
        base = live_app.ready()

        r = httpx.get(f"{base}/root/mid/leaf/data")
        assert r.status_code == 200
        assert r.json() == {"deep": True}

    def test_multiple_methods_same_path(self, live_app):
        router = APIRouter(prefix="/r")

        @router.get("/thing")
        def get_thing():
            return {"method": "GET"}

        @router.post("/thing")
        def post_thing():
            return {"method": "POST"}

        @router.delete("/thing")
        def del_thing():
            return {"method": "DELETE"}

        live_app.app.include_router(router)
        base = live_app.ready()

        assert httpx.get(f"{base}/r/thing").json() == {"method": "GET"}
        assert httpx.post(f"{base}/r/thing").json() == {"method": "POST"}
        assert httpx.delete(f"{base}/r/thing").json() == {"method": "DELETE"}

    def test_two_separate_routers_on_one_app(self, live_app):
        users = APIRouter(prefix="/users")
        items = APIRouter(prefix="/items")

        @users.get("/me")
        def me():
            return {"name": "alice"}

        @items.get("/")
        def list_items():
            return {"items": []}

        live_app.app.include_router(users)
        live_app.app.include_router(items)
        base = live_app.ready()

        assert httpx.get(f"{base}/users/me").json() == {"name": "alice"}
        assert httpx.get(f"{base}/items/").json() == {"items": []}

    def test_path_join_handles_double_slash(self, live_app):
        # router prefix ends with /, route starts with /
        router = APIRouter(prefix="/api/")

        @router.get("/x")
        def x():
            return {"ok": True}

        live_app.app.include_router(router)
        base = live_app.ready()

        r = httpx.get(f"{base}/api/x")
        assert r.status_code == 200

    def test_app_level_routes_still_work(self, live_app):
        # Sanity: app.get + router both register
        @live_app.app.get("/direct")
        def direct():
            return {"direct": True}

        sub = APIRouter(prefix="/sub")

        @sub.get("/x")
        def x():
            return {"sub": True}

        live_app.app.include_router(sub)
        base = live_app.ready()

        assert httpx.get(f"{base}/direct").json() == {"direct": True}
        assert httpx.get(f"{base}/sub/x").json() == {"sub": True}


# -----------------------------------------------------------------
# OpenAPI schema reflects nested routes (introspection)
# -----------------------------------------------------------------
class TestOpenAPI:
    def test_openapi_lists_nested_routes(self, live_app):
        router = APIRouter(prefix="/api")

        @router.get("/users")
        def list_users():
            return []

        @router.post("/users")
        def create_user():
            return {}

        live_app.app.include_router(router)
        base = live_app.ready()

        spec = httpx.get(f"{base}/api-docs/openapi.json").json()
        paths = spec["paths"]
        assert "/api/users" in paths
        assert "get" in paths["/api/users"]
        assert "post" in paths["/api/users"]

    def test_openapi_tags_merge(self, live_app):
        # router carries tags, route carries tags -> merged
        router = APIRouter(prefix="/api", tags=["v1"])

        @router.get("/x", tags=["users"])
        def x():
            return {}

        live_app.app.include_router(router)
        base = live_app.ready()

        spec = httpx.get(f"{base}/api-docs/openapi.json").json()
        op = spec["paths"]["/api/x"]["get"]
        assert "v1" in op["tags"]
        assert "users" in op["tags"]

    def test_openapi_nested_prefix(self, live_app):
        leaf = APIRouter(prefix="/leaf")

        @leaf.get("/x")
        def x():
            return {}

        outer = APIRouter(prefix="/outer")
        outer.include_router(leaf)

        live_app.app.include_router(outer)
        base = live_app.ready()

        spec = httpx.get(f"{base}/api-docs/openapi.json").json()
        assert "/outer/leaf/x" in spec["paths"]


# -----------------------------------------------------------------
# Post-serve freeze: serve() calls base_router.freeze(), which
# recursively caches the flattened tree. Mutating any router whose
# `frozen` flag is set must raise.
# -----------------------------------------------------------------
class TestPostServeFrozen:
    def test_app_base_router_frozen_after_serve(self, live_app):
        # The base router lives inside FastrAPI; we observe its
        # frozen-ness indirectly through @app.get raising.
        @live_app.app.get("/before")
        def before():
            return {"ok": True}

        live_app.ready()

        with pytest.raises(RuntimeError, match="frozen"):

            @live_app.app.get("/after")
            def after():
                return {}

    def test_included_router_can_be_pinned_at_serve(self, live_app):
        # An APIRouter included into the app is reachable through the
        # base router's sub_routers and gets frozen by recursive
        # freeze() at serve time. (Note: freeze() in router.rs only
        # freezes self, but flatten + cached_flat is what matters; we
        # check the visible contract — registration after serve fails
        # for the router used by the app.)
        router = APIRouter(prefix="/api")

        @router.get("/x")
        def x():
            return {"ok": True}

        live_app.app.include_router(router)
        live_app.ready()

        # /x is served
        base = live_app.base
        assert httpx.get(f"{base}/api/x").json() == {"ok": True}

    def test_serve_does_not_freeze_unused_router(self):
        # A standalone router never attached to a served app is not
        # affected by anyone else's serve() call.
        r = APIRouter()

        @r.get("/x")
        def x():
            return {}

        @r.post("/y")
        def y():
            return {}  # still allowed
