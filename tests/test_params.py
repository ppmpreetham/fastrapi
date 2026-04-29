from fastrapi import Body, Cookie, Depends, File, Form, Header, Path, Query, Security
from fastrapi.params import Undefined, Unset


def test_query_path_and_cookie_store_common_metadata():
    query = Query(
        "default",
        alias="q",
        title="Query title",
        description="Query description",
        gt=1,
        ge=2,
        lt=10,
        le=9,
        min_length=3,
        max_length=20,
        pattern="[a-z]+",
        deprecated=True,
        include_in_schema=False,
        examples={"ok": "abc"},
    )
    path = Path(..., alias="item-id")
    cookie = Cookie(None, alias="session-id")

    assert query.default == "default"
    assert query.alias == "q"
    assert query.title == "Query title"
    assert query.description == "Query description"
    assert query.gt == 1
    assert query.ge == 2
    assert query.lt == 10
    assert query.le == 9
    assert query.min_length == 3
    assert query.max_length == 20
    assert query.pattern == "[a-z]+"
    assert query.deprecated is True
    assert query.include_in_schema is False
    assert query.examples == {"ok": "abc"}
    assert path.alias == "item-id"
    assert cookie.default is None
    assert cookie.alias == "session-id"


def test_header_body_form_and_file_specific_fields():
    header = Header("token", convert_underscores=False)
    body = Body(..., embed=True, media_type="application/custom+json")
    form = Form("value")
    file = File(None)

    assert header.default == "token"
    assert header.convert_underscores is False
    assert body.default is Ellipsis
    assert body.embed is True
    assert body.media_type == "application/custom+json"
    assert form.media_type == "application/x-www-form-urlencoded"
    assert file.media_type == "multipart/form-data"


def test_depends_and_security_store_dependency_options():
    def dependency():
        return "value"

    depends = Depends(dependency, use_cache=False)
    security = Security(dependency, scopes=["read", "write"], use_cache=True)

    assert depends.dependency is dependency
    assert depends.use_cache is False
    assert security.dependency is dependency
    assert security.use_cache is True
    assert security.scopes == ["read", "write"]


def test_sentinel_classes_can_be_instantiated():
    assert type(Unset()).__name__ == "Unset"
    assert type(Undefined()).__name__ == "Undefined"
