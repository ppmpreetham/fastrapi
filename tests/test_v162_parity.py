import fastapi
import pytest

import fastrapi


def both(run):
    assert run(fastrapi) == run(fastapi)


def raises_type_error(call):
    try:
        call()
    except TypeError:
        return True
    return False


@pytest.mark.parametrize("marker", ["Query", "Header", "Cookie", "Path", "Body", "Form", "File"])
def test_param_functions_reject_unknown_keyword_arguments(marker):
    both(lambda lib: raises_type_error(lambda: getattr(lib, marker)(foo=1)))


@pytest.mark.parametrize("marker", ["Query", "Header", "Body", "Form"])
def test_param_functions_accept_pydantic_field_options(marker):
    def run(lib):
        info = getattr(lib, marker)(None, validate_default=True, union_mode="left_to_right", fail_fast=True)
        return info.validate_default, repr(info), info.json_schema_extra

    both(run)
