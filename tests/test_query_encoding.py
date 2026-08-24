# Locks in the query-string decoding contract: '&' separates pairs, '+' means a
# space, %XX becomes an octet, malformed escapes are left verbatim, and UTF-8 is
# decoded lossily. `form_urlencoded::parse` honours all of these exactly as the
# hand-rolled decoder it replaced did.


def test_percent_encoded(client, app):
    @app.get("/q")
    def q(name: str | None = None, tag: str | None = None):
        return {"name": name, "tag": tag}

    assert client.get("/q?name=hello%20world").json() == {
        "name": "hello world",
        "tag": None,
    }
    assert client.get("/q?name=a%2Bb&tag=x%26y").json() == {
        "name": "a+b",
        "tag": "x&y",
    }
    # '+' means space
    assert client.get("/q?name=hello+world").json() == {"name": "hello world", "tag": None}


def test_empty_and_malformed_segments(client, app):
    @app.get("/q")
    def q(name: str | None = None):
        return {"name": name}

    # empty segments are skipped
    assert client.get("/q?&&name=x&").json() == {"name": "x"}
    # a bare '=' yields an empty value
    assert client.get("/q?name=").json() == {"name": ""}
    # a malformed escape is left verbatim
    assert client.get("/q?name=%ZZ").json() == {"name": "%ZZ"}
    # truncated escape left verbatim
    assert client.get("/q?name=%4").json() == {"name": "%4"}


def test_utf8_and_semicolon(client, app):
    @app.get("/q")
    def q(name: str | None = None):
        return {"name": name}

    # percent-encoded UTF-8 round-trips
    assert client.get("/q?name=%E4%B8%AD%E6%96%87").json() == {"name": "\u4e2d\u6587"}
    # ';' is NOT a separator (only '&' is)
    assert client.get("/q?name=a;b").json() == {"name": "a;b"}


def test_repeated_and_no_value(client, app):
    @app.get("/items/")
    def items(q: str | None = None):
        return {"q": q}

    # repeated key: the first occurrence wins (same as before)
    assert client.get("/items/?q=one&q=two").json() == {"q": "one"}
    # key with no '=' at all
    assert client.get("/items/?q").json() == {"q": ""}
