import dataclasses

from pydantic import BaseModel, ValidationError


def _prepare_response_content(res, *, exclude_unset, exclude_defaults, exclude_none):
    if isinstance(res, BaseModel):
        return res.model_dump(
            by_alias=True,
            exclude_unset=exclude_unset,
            exclude_defaults=exclude_defaults,
            exclude_none=exclude_none,
        )
    if isinstance(res, list):
        return [
            _prepare_response_content(
                item,
                exclude_unset=exclude_unset,
                exclude_defaults=exclude_defaults,
                exclude_none=exclude_none,
            )
            for item in res
        ]
    if isinstance(res, dict):
        return {
            key: _prepare_response_content(
                value,
                exclude_unset=exclude_unset,
                exclude_defaults=exclude_defaults,
                exclude_none=exclude_none,
            )
            for key, value in res.items()
        }
    if dataclasses.is_dataclass(res) and not isinstance(res, type):
        return dataclasses.asdict(res)
    return res


def _validate_response(adapter, content):
    try:
        return adapter.validate_python(content, from_attributes=True)
    except ValidationError as exc:
        from fastrapi.exceptions import ResponseValidationError

        errors = [
            {**error, 'loc': ('response', *error['loc'])}
            for error in exc.errors(include_url=False)
        ]
        raise ResponseValidationError(errors, body=content) from None


def validate_response(
    adapter,
    content,
    include=None,
    exclude=None,
    by_alias=False,
    exclude_unset=False,
    exclude_defaults=False,
    exclude_none=False,
):
    prepared = _prepare_response_content(
        content,
        exclude_unset=exclude_unset,
        exclude_defaults=exclude_defaults,
        exclude_none=exclude_none,
    )
    return _validate_response(adapter, prepared)


def serialize_response(
    adapter,
    content,
    include=None,
    exclude=None,
    by_alias=False,
    exclude_unset=False,
    exclude_defaults=False,
    exclude_none=False,
):
    value = validate_response(
        adapter,
        content,
        exclude_unset=exclude_unset,
        exclude_defaults=exclude_defaults,
        exclude_none=exclude_none,
    )
    return adapter.dump_json(
        value,
        include=include,
        exclude=exclude,
        by_alias=by_alias,
        exclude_unset=exclude_unset,
        exclude_defaults=exclude_defaults,
        exclude_none=exclude_none,
    )
