from pydantic import ValidationError


def response_error(exc, content):
    """Turns a failed response validation into FastAPI's `ResponseValidationError`."""
    if not isinstance(exc, ValidationError):
        return exc
    from fastrapi.exceptions import ResponseValidationError

    errors = [
        {**error, 'loc': ('response', *error['loc'])}
        for error in exc.errors(include_url=False)
    ]
    return ResponseValidationError(errors, body=content)


def lazy_codec(adapter):
    """The methods of an adapter whose schema is not built yet. pydantic retries the build
    on each call and reads the calling frame for it, so the calls come from Python."""
    return (
        lambda value, **options: adapter.validate_python(value, **options),
        lambda value, **options: adapter.dump_json(value, **options),
        lambda value, **options: adapter.dump_python(value, **options),
    )


def _validate_response(adapter, content):
    try:
        return adapter.validate_python(content, from_attributes=True)
    except ValidationError as exc:
        raise response_error(exc, content) from None
