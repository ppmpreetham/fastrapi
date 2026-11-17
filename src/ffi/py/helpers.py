import abc
import dataclasses
import datetime
import importlib
import re
import warnings
from collections import defaultdict, deque
from collections.abc import Callable
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
from typing import Annotated, Any
from uuid import UUID

import anyio
import anyio.to_thread
from pydantic import AnyUrl, NameEmail, SecretBytes, SecretStr
from pydantic import TypeAdapter, ValidationError
from pydantic.fields import FieldInfo
from pydantic.main import IncEx
from pydantic_core import PydanticUndefined, Url
from starlette.concurrency import run_in_threadpool


def install_lazy_attributes(module, sources):
    """Imports each attribute of `module` from its source module on first use.

    Some Starlette modules warn when they are imported, so importing fastrapi must not import them.
    """

    def __getattr__(name):
        try:
            source = sources[name]
        except KeyError:
            raise AttributeError(f"module {module.__name__!r} has no attribute {name!r}") from None
        value = getattr(importlib.import_module(source), name)
        setattr(module, name, value)
        return value

    module.__getattr__ = __getattr__


try:
    from pydantic_extra_types.color import Color as PyExtraColor
except ImportError:

    class PyExtraColor:
        pass


def get_path_param_names(path):
    return set(re.findall("{(.*?)}", path))


def is_pydantic_v1_model_instance(obj):
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            from pydantic import v1
    except ImportError:
        return False
    return isinstance(obj, v1.BaseModel)


def isoformat(o: datetime.date | datetime.time) -> str:
    return o.isoformat()


def decimal_encoder(dec_value: Decimal) -> int | float:
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


def generate_encoders_by_class_tuples(
    type_encoder_map: dict[Any, Callable[[Any], Any]],
) -> dict[Callable[[Any], Any], tuple[Any, ...]]:
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


def _not_authenticated(headers):
    from fastrapi.exceptions import HTTPException

    return HTTPException(status_code=401, detail="Not authenticated", headers=headers)


class APIKeyBase(SecurityBase):
    def __init__(self, location, name, description, scheme_name, auto_error):
        from fastrapi.openapi.models import APIKey

        self.auto_error = auto_error
        self.model = APIKey(**{"in": location}, name=name, description=description)
        self.scheme_name = scheme_name or self.__class__.__name__

    def make_not_authenticated_error(self):
        return _not_authenticated({"WWW-Authenticate": "APIKey"})

    def check_api_key(self, api_key):
        if not api_key:
            if self.auto_error:
                raise self.make_not_authenticated_error()
            return None
        return api_key


class HTTPBase(SecurityBase):
    def __init__(self, *, scheme, scheme_name=None, description=None, auto_error=True):
        from fastrapi.openapi.models import HTTPBase as HTTPBaseModel

        self.model = HTTPBaseModel(scheme=scheme, description=description)
        self.scheme_name = scheme_name or self.__class__.__name__
        self.auto_error = auto_error

    def make_authenticate_headers(self):
        return {"WWW-Authenticate": self.model.scheme.title()}

    def make_not_authenticated_error(self):
        return _not_authenticated(self.make_authenticate_headers())

    async def __call__(self, request):
        from fastrapi.security import HTTPAuthorizationCredentials

        scheme, _, credentials = (request.headers.get("Authorization") or "").partition(" ")
        if not (scheme and credentials.strip()):
            if self.auto_error:
                raise self.make_not_authenticated_error()
            return None
        return HTTPAuthorizationCredentials(scheme=scheme, credentials=credentials.strip())


class AsyncExitStackMiddleware:
    def __init__(self, app, context_name="fastapi_middleware_astack"):
        self.app = app
        self.context_name = context_name

    async def __call__(self, scope, receive, send):
        async with AsyncExitStack() as stack:
            scope[self.context_name] = stack
            await self.app(scope, receive, send)


@dataclasses.dataclass
class ModelField:
    field_info: FieldInfo
    name: str
    mode: str = "validation"
    config: Any = None

    def __post_init__(self):
        self._type_adapter = TypeAdapter(Annotated[self.field_info.annotation, self.field_info], config=self.config)

    @property
    def alias(self):
        return self.field_info.alias if self.field_info.alias is not None else self.name

    @property
    def validation_alias(self):
        alias = self.field_info.validation_alias
        return alias if isinstance(alias, str) and alias else None

    @property
    def serialization_alias(self):
        return self.field_info.serialization_alias or None

    @property
    def default(self):
        return self.get_default()

    def get_default(self):
        if self.field_info.is_required():
            return PydanticUndefined
        return self.field_info.get_default(call_default_factory=True)

    def validate(self, value, values={}, *, loc=()):
        try:
            return self._type_adapter.validate_python(value, from_attributes=True), []
        except ValidationError as exc:
            return None, [{**error, "loc": loc + error.get("loc", ())} for error in exc.errors(include_url=False)]

    def serialize(self, value, *, mode="json", **options):
        return self._type_adapter.dump_python(value, mode=mode, **options)

    def serialize_json(self, value, **options):
        return self._type_adapter.dump_json(value, **options)

    def __hash__(self):
        return id(self)


def create_model_field(name, type_, default=PydanticUndefined, field_info=None, alias=None, mode="validation"):
    field_info = field_info or FieldInfo(annotation=type_, default=default, alias=alias)
    return ModelField(mode=mode, name=name, field_info=field_info)


def docs_routes(app):
    from starlette.routing import Route

    from fastrapi.openapi.docs import get_redoc_html, get_swagger_ui_html, get_swagger_ui_oauth2_redirect_html
    from fastrapi.responses import JSONResponse

    if not app.openapi_url:
        return []

    def openapi_url(request):
        return request.scope.get("root_path", "").rstrip("/") + app.openapi_url

    async def openapi(request):
        return JSONResponse(app.openapi())

    async def swagger_ui_html(request):
        root = request.scope.get("root_path", "").rstrip("/")
        redirect = app.swagger_ui_oauth2_redirect_url
        return get_swagger_ui_html(
            openapi_url=openapi_url(request),
            title=f"{app.title} - Swagger UI",
            oauth2_redirect_url=root + redirect if redirect else None,
            init_oauth=app.swagger_ui_init_oauth,
            swagger_ui_parameters=app.swagger_ui_parameters,
        )

    async def swagger_ui_redirect(request):
        return get_swagger_ui_oauth2_redirect_html()

    async def redoc_html(request):
        return get_redoc_html(openapi_url=openapi_url(request), title=f"{app.title} - ReDoc")

    routes = [Route(app.openapi_url, openapi, include_in_schema=False)]
    if app.docs_url:
        routes.append(Route(app.docs_url, swagger_ui_html, include_in_schema=False))
        if app.swagger_ui_oauth2_redirect_url:
            routes.append(Route(app.swagger_ui_oauth2_redirect_url, swagger_ui_redirect, include_in_schema=False))
    if app.redoc_url:
        routes.append(Route(app.redoc_url, redoc_html, include_in_schema=False))
    return routes


async def asgi_call(self, scope, receive, send):
    stack = self._middleware_stack_override
    await (self._asgi if stack is None else stack)(scope, receive, send)


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
