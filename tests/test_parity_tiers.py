import fastapi
import pytest

import fastrapi


@pytest.mark.parametrize("code", [None, "default", "2XX", 100, 200, 204, 205, 304, "201", 404])
def test_body_allowed_for_status_code(code):
    from fastapi.utils import is_body_allowed_for_status_code as expected
    from fastrapi.utils import is_body_allowed_for_status_code as actual

    assert actual(code) == expected(code)
