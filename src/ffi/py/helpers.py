import abc
import datetime
import re
from collections import defaultdict, deque
from contextlib import AsyncExitStack, asynccontextmanager
from decimal import Decimal
from enum import Enum
from ipaddress import (
    IPv4Address,
    IPv4Interface,
    IPv4Network,
    IPv6Address,
    IPv6Interface,
    IPv6Network,
)
from pathlib import Path
from types import GeneratorType
from uuid import UUID

import anyio
import anyio.to_thread
from pydantic import AnyUrl, NameEmail, SecretBytes, SecretStr
from pydantic_core import Url
from starlette.concurrency import run_in_threadpool


def isoformat(o):
    return o.isoformat()


def decimal_encoder(dec_value):
    exponent = dec_value.as_tuple().exponent
    if isinstance(exponent, int) and exponent >= 0:
        return int(dec_value)
    return float(dec_value)


ENCODERS_BY_TYPE = {
    bytes: lambda o: o.decode(),
    datetime.date: isoformat,
    datetime.datetime: isoformat,
    datetime.time: isoformat,
    datetime.timedelta: lambda td: td.total_seconds(),
    Decimal: decimal_encoder,
    Enum: lambda o: o.value,
    frozenset: list,
    deque: list,
    GeneratorType: list,
    IPv4Address: str,
    IPv4Interface: str,
    IPv4Network: str,
    IPv6Address: str,
    IPv6Interface: str,
    IPv6Network: str,
    NameEmail: str,
    Path: str,
    re.Pattern: lambda o: o.pattern,
    SecretBytes: str,
    SecretStr: str,
    set: list,
    UUID: str,
    Url: str,
    AnyUrl: str,
}


def generate_encoders_by_class_tuples(type_encoder_map):
    encoders_by_class_tuples = defaultdict(tuple)
    for type_, encoder in type_encoder_map.items():
        encoders_by_class_tuples[encoder] += (type_,)
    return encoders_by_class_tuples


encoders_by_class_tuples = generate_encoders_by_class_tuples(ENCODERS_BY_TYPE)


def _async_lifespan(factory):
    import inspect
    from contextlib import contextmanager

    if inspect.isasyncgenfunction(factory):
        return asynccontextmanager(factory)
    if inspect.isgeneratorfunction(factory):
        sync_factory = contextmanager(factory)

        @asynccontextmanager
        async def wrapped(app):
            with sync_factory(app) as state:
                yield state

        return wrapped
    return factory


def merge_lifespan_context(original, nested):
    original = _async_lifespan(original)
    nested = _async_lifespan(nested)

    @asynccontextmanager
    async def merged_lifespan(app):
        async with original(app) as maybe_original_state:
            async with nested(app) as maybe_nested_state:
                if maybe_nested_state is None and maybe_original_state is None:
                    yield None
                else:
                    yield {**(maybe_nested_state or {}), **(maybe_original_state or {})}

    return merged_lifespan


def handlers_lifespan(on_startup, on_shutdown):
    import inspect

    @asynccontextmanager
    async def lifespan(app):
        for handler in on_startup or []:
            result = handler()
            if inspect.isawaitable(result):
                await result
        yield None
        for handler in on_shutdown or []:
            result = handler()
            if inspect.isawaitable(result):
                await result

    return lifespan


def default_lifespan(router):
    return handlers_lifespan(router.on_startup, router.on_shutdown)


def request_endpoint(endpoint):
    import inspect

    from fastrapi import Request

    if inspect.iscoroutinefunction(endpoint):

        async def route(request: Request):
            return await endpoint(request)

    else:

        def route(request: Request):
            return endpoint(request)

    route.__name__ = getattr(endpoint, "__name__", "route")
    route.__doc__ = endpoint.__doc__
    return route


class SecurityBase(abc.ABC):
    pass


class APIKeyBase(SecurityBase):
    pass


class HTTPBase(SecurityBase):
    pass


class AsyncExitStackMiddleware:
    def __init__(self, app, context_name="fastapi_middleware_astack"):
        self.app = app
        self.context_name = context_name

    async def __call__(self, scope, receive, send):
        async with AsyncExitStack() as stack:
            scope[self.context_name] = stack
            await self.app(scope, receive, send)


@asynccontextmanager
async def contextmanager_in_threadpool(cm):
    exit_limiter = anyio.CapacityLimiter(1)
    try:
        yield await run_in_threadpool(cm.__enter__)
    except Exception as e:
        ok = bool(
            await anyio.to_thread.run_sync(
                cm.__exit__, type(e), e, e.__traceback__, limiter=exit_limiter
            )
        )
        if not ok:
            raise e
    else:
        await anyio.to_thread.run_sync(
            cm.__exit__, None, None, None, limiter=exit_limiter
        )


def route_decorator(add_route, path, methods, name, include_in_schema):
    def decorator(func):
        add_route(path, func, methods=methods, name=name, include_in_schema=include_in_schema)
        return func

    return decorator
