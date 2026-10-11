import pytest

fastrapi = pytest.importorskip("fastrapi")


def test_response_api_has_mutable_raw_headers():
    response = fastrapi.Response("ok")
    response.set_cookie("a", "1")
    assert response.raw_headers[-1][0] == b"set-cookie"
    assert b"a=1" in response.raw_headers[-1][1]


def test_response_api_render_and_charset():
    response = fastrapi.Response("ok", media_type="text/plain")
    assert response.body == b"ok"
    assert response.charset == "utf-8"


@pytest.mark.parametrize("factory", [
    lambda: fastrapi.responses.HTMLResponse("ok"),
    lambda: fastrapi.responses.JSONResponse({"ok": True}),
    lambda: fastrapi.responses.ORJSONResponse({"ok": True}),
    lambda: fastrapi.responses.UJSONResponse({"ok": True}),
    lambda: fastrapi.responses.PlainTextResponse("ok"),
    lambda: fastrapi.responses.StreamingResponse(iter([b"ok"])),
    lambda: fastrapi.responses.RedirectResponse("/ok"),
])
def test_response_classes_expose_cookie_api(factory):
    response = factory()
    response.set_cookie("a", "1")
    response.delete_cookie("a")
    assert hasattr(response, "headers")
    assert len([item for item in response.raw_headers if item[0] == b"set-cookie"]) == 2


def test_file_response_exposes_cookie_api(tmp_path):
    path = tmp_path / "a.txt"
    path.write_text("ok")
    response = fastrapi.responses.FileResponse(path)
    response.set_cookie("a", "1")
    assert response.raw_headers[-1][0] == b"set-cookie"
