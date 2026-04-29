import pytest

from fastrapi.request import HTTPConnection, Request


def build_scope(method: str = "POST", **extra):
    scope = {
        "type": "http",
        "method": method,
        "headers": [(b"content-length", b"32")],
    }
    scope.update(extra)
    return scope


def make_receive(messages, counter):
    async def receive():
        counter["count"] += 1
        if messages:
            return messages.pop(0)
        return {"type": "http.request", "body": b"", "more_body": False}

    return receive


def test_request_rejects_invalid_scope_type():
    with pytest.raises(ValueError, match="Scope type"):
        Request({"type": "lifespan"})


def test_request_exposes_scope_receive_and_send():
    async def receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(message):
        return message

    scope = build_scope()
    request = Request(scope, receive=receive, send=send)

    assert request.scope is scope
    assert request.receive is receive
    assert request.send is send


def test_request_properties_default_to_empty_mappings():
    request = Request(build_scope(client=("127.0.0.1", 1234)))

    assert request.client == ("127.0.0.1", 1234)
    assert request.headers == [(b"content-length", b"32")]
    assert request.path_params == {}
    assert request.query_params == {}
    assert request.cookies == {}


def test_request_state_is_created_and_cached_on_scope():
    scope = build_scope()
    request = Request(scope)

    request.state.user_id = 42

    assert request.state.user_id == 42
    assert scope["state"] is request.state


def test_request_body_is_cached(await_result):
    counter = {"count": 0}
    body = b'{"hello": "world"}'
    receive = make_receive(
        [{"type": "http.request", "body": body, "more_body": False}],
        counter,
    )
    request = Request(build_scope("POST"), receive=receive)

    async def scenario():
        assert await request.body() == body
        assert await request.body() == body

    await_result(scenario)
    assert counter["count"] == 1


def test_request_body_combines_chunks(await_result):
    counter = {"count": 0}
    receive = make_receive(
        [
            {"type": "http.request", "body": b'{"a": ', "more_body": True},
            {"type": "http.request", "body": b"1}", "more_body": False},
        ],
        counter,
    )
    request = Request(build_scope("POST"), receive=receive)

    async def scenario():
        assert await request.body() == b'{"a": 1}'

    await_result(scenario)
    assert counter["count"] == 2


def test_request_json_reads_cached_body(await_result):
    counter = {"count": 0}
    body = b'{"count": 3}'
    receive = make_receive(
        [{"type": "http.request", "body": body, "more_body": False}],
        counter,
    )
    request = Request(build_scope("POST"), receive=receive)

    async def scenario():
        assert await request.json() == {"count": 3}
        assert await request.body() == body

    await_result(scenario)
    assert counter["count"] == 1


def test_request_body_empty_for_methods_without_payload(await_result):
    counter = {"count": 0}
    request = Request(build_scope("GET"), receive=make_receive([], counter))

    assert request.body() == b""
    assert counter["count"] == 0


def test_http_connection_exposes_scope():
    scope = {"type": "websocket", "path": "/ws"}
    connection = HTTPConnection(scope)

    assert connection.scope is scope
