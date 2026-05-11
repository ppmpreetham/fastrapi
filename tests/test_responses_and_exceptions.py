from fastrapi.background import BackgroundTasks
from fastrapi.datastructures import UploadFile
from fastrapi.exceptions import (
    HTTPException,
    RequestValidationError,
    ResponseValidationError,
    ValidationException,
    WebSocketException,
)
from fastrapi.responses import HTMLResponse, JSONResponse, PlainTextResponse, RedirectResponse


def test_response_classes_store_content_and_status():
    assert JSONResponse({"ok": True}, status_code=201).content == {"ok": True}
    assert JSONResponse({"ok": True}, status_code=201).status_code == 201
    assert HTMLResponse("<p>ok</p>").content == "<p>ok</p>"
    assert HTMLResponse("<p>ok</p>").status_code == 200
    assert PlainTextResponse("ok", status_code=202).content == "ok"
    assert PlainTextResponse("ok", status_code=202).status_code == 202
    assert RedirectResponse("/target").url == "/target"
    assert RedirectResponse("/target").status_code == 307


def test_http_exception_attributes_and_repr():
    exc = HTTPException(status_code=404, detail="missing", headers={"x": "y"})

    assert exc.status_code == 404
    assert exc.detail == "missing"
    assert exc.headers == {"x": "y"}
    assert str(exc) == "404: missing"
    assert repr(exc) == "HTTPException(status_code=404, detail='missing')"


def test_validation_exception_errors_and_body():
    errors = [{"loc": ["body", "name"], "msg": "required"}]
    exc = ValidationException(errors)
    request_exc = RequestValidationError(errors, body={"name": None})
    response_exc = ResponseValidationError(errors)

    assert exc.errors() == errors
    assert str(exc) == "1 validation error occurred"
    assert request_exc.errors() == errors
    assert request_exc.body == {"name": None}
    assert response_exc.errors() == errors
    assert response_exc.body is None


def test_websocket_exception_attributes_and_repr():
    exc = WebSocketException(code=1008, reason="policy")

    assert exc.code == 1008
    assert exc.reason == "policy"
    assert str(exc) == "1008: policy"
    assert repr(exc) == 'WebSocketException(code=1008, reason=Some("policy"))'


def test_background_tasks_accept_callables():
    calls = []

    def task(value):
        calls.append(value)

    tasks = BackgroundTasks()
    tasks.add_task(task, ["done"])

    # The public Python API currently only queues tasks; execution happens from
    # the Rust response path.
    assert calls == []


def test_upload_file_async_read_write_seek_close(await_result):
    upload = UploadFile(None, filename="data.txt", content_type="text/plain")

    assert upload.filename == "data.txt"
    assert upload.content_type == "text/plain"
    assert upload.size is None

    async def scenario():
        await upload.write(b"abcdef")
        assert upload.size == 6
        assert await upload.read(2) == b"ab"
        await upload.seek(1)
        assert await upload.read(None) == b"bcdef"
        assert await upload.close() == ()

    await_result(scenario)
    assert upload.size == 6
