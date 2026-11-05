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
    prepared = _prepare_response_content(
        content,
        exclude_unset=exclude_unset,
        exclude_defaults=exclude_defaults,
        exclude_none=exclude_none,
    )
    value = _validate_response(adapter, prepared)
    return adapter.dump_json(
        value,
        include=include,
        exclude=exclude,
        by_alias=by_alias,
        exclude_unset=exclude_unset,
        exclude_defaults=exclude_defaults,
        exclude_none=exclude_none,
    )


def stream_factory(mode, item_type, response_class, options):
    import json

    from pydantic import TypeAdapter

    from fastrapi.encoders import jsonable_encoder

    adapter = TypeAdapter(item_type) if item_type is not None else None
    options = dict(options or {})

    def validated(item):
        return _validate_response(adapter, item)

    def serialize(item):
        if adapter is not None:
            return adapter.dump_json(validated(item), **options)
        return json.dumps(jsonable_encoder(item)).encode('utf-8')

    def sse_value(item):
        from fastrapi.sse import ServerSentEvent

        if isinstance(item, ServerSentEvent) or adapter is None:
            return item
        return adapter.dump_python(validated(item), mode='json', **options)

    def factory(gen):
        from fastrapi.responses import StreamingResponse

        if mode == 'raw':
            return response_class(gen)
        convert = serialize if mode == 'jsonl' else sse_value
        suffix = b'\n' if mode == 'jsonl' else None
        if hasattr(gen, '__anext__'):

            async def body():
                async for item in gen:
                    value = convert(item)
                    yield value + suffix if suffix is not None else value

        else:

            def body():
                for item in gen:
                    value = convert(item)
                    yield value + suffix if suffix is not None else value

        if mode == 'jsonl':
            return StreamingResponse(body(), media_type='application/jsonl')
        return response_class(
            body(),
            headers={'cache-control': 'no-cache', 'x-accel-buffering': 'no'},
        )

    return factory
