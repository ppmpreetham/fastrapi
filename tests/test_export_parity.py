import pytest

fastrapi = pytest.importorskip("fastrapi")


def test_public_response_exports():
    assert fastrapi.responses.Response is fastrapi.Response
    assert fastrapi.responses.JSONResponse
    assert fastrapi.responses.StreamingResponse


def test_sse_exports():
    assert fastrapi.sse.KEEPALIVE_COMMENT == b": ping\n\n"
    assert fastrapi.sse.format_sse_event(data_str="x") == b"data: x\n\n"


def test_sse_server_event_model_behavior():
    event = fastrapi.sse.ServerSentEvent(data={"x": 1}, event="message", retry=5)
    assert event.model_dump()["event"] == "message"
    assert fastrapi.sse.ServerSentEvent.model_validate({"data": "x"}).data == "x"
