import fastapi
import pytest

import fastrapi


@pytest.mark.parametrize("code", [None, "default", "2XX", 100, 200, 204, 205, 304, "201", 404])
def test_body_allowed_for_status_code(code):
    from fastapi.utils import is_body_allowed_for_status_code as expected
    from fastrapi.utils import is_body_allowed_for_status_code as actual

    assert actual(code) == expected(code)


def test_status_module_matches_fastapi():
    import warnings

    from fastrapi import status

    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        assert status.HTTP_422_UNPROCESSABLE_ENTITY == fastapi.status.HTTP_422_UNPROCESSABLE_ENTITY
    assert status.HTTP_418_IM_A_TEAPOT == 418 and status.WS_1008_POLICY_VIOLATION == 1008
