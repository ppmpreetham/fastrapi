import dataclasses
import inspect
from contextlib import asynccontextmanager, contextmanager
from typing import Any, Callable, Literal, Optional, Sequence, get_origin

from pydantic import BaseModel, create_model
from pydantic_core import PydanticUndefined
from starlette.background import BackgroundTasks as StarletteBackgroundTasks
from starlette.concurrency import run_in_threadpool
from starlette.requests import HTTPConnection, Request
from starlette.responses import Response as StarletteResponse
from starlette.websockets import WebSocket


@dataclasses.dataclass
class Dependant:
    path_params: list = dataclasses.field(default_factory=list)
    query_params: list = dataclasses.field(default_factory=list)
    header_params: list = dataclasses.field(default_factory=list)
    cookie_params: list = dataclasses.field(default_factory=list)
    body_params: list = dataclasses.field(default_factory=list)
    dependencies: list = dataclasses.field(default_factory=list)
    name: Optional[str] = None
    call: Optional[Callable[..., Any]] = None
    request_param_name: Optional[str] = None
    websocket_param_name: Optional[str] = None
    http_connection_param_name: Optional[str] = None
    response_param_name: Optional[str] = None
    background_tasks_param_name: Optional[str] = None
    security_scopes_param_name: Optional[str] = None
    own_oauth_scopes: Optional[list] = None
    parent_oauth_scopes: Optional[list] = None
    use_cache: bool = True
    path: Optional[str] = None
    scope: Optional[Literal["function", "request"]] = None

    @property
    def oauth_scopes(self):
        scopes = list(self.parent_oauth_scopes or [])
        scopes.extend(scope for scope in self.own_oauth_scopes or [] if scope not in scopes)
        return scopes

    @property
    def cache_key(self):
        return (self.call, tuple(sorted(set(self.oauth_scopes))))


@dataclasses.dataclass
class SecurityRequirement:
    security_scheme: Any
    scopes: Optional[Sequence[str]] = None


@dataclasses.dataclass
class SolvedDependency:
    values: dict
    errors: list
    background_tasks: Optional[StarletteBackgroundTasks]
    response: StarletteResponse
    dependency_cache: dict


def _subclass(value, *classes):
    return inspect.isclass(value) and any(issubclass(value, cls) for cls in classes)


def _fastrapi():
    import fastrapi
    import fastrapi.params
    import fastrapi.security

    return fastrapi


def _model_field(field_info, name, mode="validation"):
    from fastrapi._helpers import ModelField

    return ModelField(field_info=field_info, name=name, mode=mode)


def get_dependant(*, path, call, name=None, own_oauth_scopes=None, parent_oauth_scopes=None, use_cache=True, scope=None):
    from fastrapi.dependencies.utils import analyze_param, get_path_param_names, get_typed_signature

    dependant = Dependant(
        call=call,
        name=name,
        path=path,
        use_cache=use_cache,
        scope=scope,
        own_oauth_scopes=own_oauth_scopes,
        parent_oauth_scopes=parent_oauth_scopes,
    )
    current_scopes = (parent_oauth_scopes or []) + (own_oauth_scopes or [])
    path_param_names = get_path_param_names(path)
    for param_name, param in get_typed_signature(call).parameters.items():
        details = analyze_param(
            param_name=param_name,
            annotation=param.annotation,
            value=param.default,
            path_param_names=path_param_names,
        )
        if details.depends is not None:
            scopes = list(getattr(details.depends, "scopes", None) or [])
            dependant.dependencies.append(
                get_dependant(
                    path=path,
                    call=details.depends.dependency,
                    name=param_name,
                    own_oauth_scopes=scopes,
                    parent_oauth_scopes=current_scopes,
                    use_cache=details.depends.use_cache,
                    scope=getattr(details.depends, "scope", None),
                )
            )
        elif not add_non_field_param_to_dependency(
            param_name=param_name, type_annotation=details.type_annotation, dependant=dependant
        ):
            if isinstance(details.field.field_info, _fastrapi().params.Body):
                dependant.body_params.append(details.field)
            else:
                add_param_to_fields(field=details.field, dependant=dependant)
    return dependant


def add_non_field_param_to_dependency(*, param_name, type_annotation, dependant):
    fastrapi = _fastrapi()
    slots = (
        ("request_param_name", (Request, fastrapi.Request)),
        ("websocket_param_name", (WebSocket, fastrapi.WebSocket)),
        ("http_connection_param_name", (HTTPConnection,)),
        ("response_param_name", (StarletteResponse, fastrapi.Response)),
        ("background_tasks_param_name", (StarletteBackgroundTasks, fastrapi.BackgroundTasks)),
        ("security_scopes_param_name", (fastrapi.security.SecurityScopes,)),
    )
    for slot, classes in slots:
        if _subclass(type_annotation, *classes):
            setattr(dependant, slot, param_name)
            return True
    return None


def add_param_to_fields(*, field, dependant):
    location = getattr(getattr(field.field_info, "in_", None), "value", None)
    target = {
        "path": dependant.path_params,
        "query": dependant.query_params,
        "header": dependant.header_params,
        "cookie": dependant.cookie_params,
    }.get(location)
    assert target is not None, f"non-body parameters must be in path, query, header or cookie: {field.name}"
    target.append(field)


def get_parameterless_sub_dependant(*, depends, path):
    assert callable(depends.dependency), "A parameter-less dependency must have a callable dependency"
    return get_dependant(
        path=path,
        call=depends.dependency,
        scope=getattr(depends, "scope", None),
        own_oauth_scopes=list(getattr(depends, "scopes", None) or []),
    )


def _flat_param_fields(fields):
    flat = []
    for field in fields:
        annotation = field.field_info.annotation
        if _subclass(annotation, BaseModel):
            flat.extend(_model_field(info, name) for name, info in annotation.model_fields.items())
        else:
            flat.append(field)
    return flat


def get_flat_params(dependant):
    groups = {"path_params": [], "query_params": [], "header_params": [], "cookie_params": []}
    visited = []
    pending = [dependant]
    while pending:
        current = pending.pop()
        if current.cache_key in visited:
            continue
        visited.append(current.cache_key)
        for group, fields in groups.items():
            fields.extend(getattr(current, group))
        pending.extend(reversed(current.dependencies))
    return [field for fields in groups.values() for field in _flat_param_fields(fields)]


def _flat_body_params(dependant):
    body = list(dependant.body_params)
    for sub in dependant.dependencies:
        body.extend(field for field in _flat_body_params(sub) if field not in body)
    return body


def should_embed_body_fields(fields):
    if not fields:
        return False
    if len({field.name for field in fields}) > 1:
        return True
    first = fields[0].field_info
    form = isinstance(first, _fastrapi().params.Form)
    return bool(getattr(first, "embed", None)) or (form and not _subclass(first.annotation, BaseModel))


def get_body_field(*, dependant, name):
    fields = _flat_body_params(dependant)
    if not fields:
        return None
    if not should_embed_body_fields(fields):
        return fields[0]
    model = create_model(name, **{field.alias: (field.field_info.annotation, field.field_info) for field in fields})
    required = any(field.field_info.is_required() for field in fields)
    body_class = type(fields[0].field_info) if isinstance(fields[0].field_info, _fastrapi().params.Form) else _fastrapi().params.Body
    info = body_class(annotation=model, default=PydanticUndefined if required else None)
    return _model_field(info, "body")


def route_dependant(route):
    dependant = get_dependant(path=route.path_format, call=route.endpoint, scope="function")
    for depends in reversed(route.dependencies or []):
        dependant.dependencies.insert(0, get_parameterless_sub_dependant(depends=depends, path=route.path_format))
    return dependant


def route_body_field(route):
    return get_body_field(dependant=route.dependant, name=f"Body_{route.unique_id}")


def route_response_field(route):
    from fastrapi._helpers import create_model_field

    model = route.response_model
    if model is None or isinstance(model, type(None)) or getattr(model, "value", model) is None:
        return None
    return create_model_field(name=f"Response_{route.unique_id}", type_=getattr(model, "value", model), mode="serialization")


def request_params_to_args(fields, received_params):
    values, errors = {}, []
    getlist = getattr(received_params, "getlist", None)
    for field in fields:
        alias = field.alias
        sequence = get_origin(field.field_info.annotation) in (list, set, tuple, frozenset)
        value = getlist(alias) if sequence and getlist else received_params.get(alias)
        location = getattr(getattr(field.field_info, "in_", None), "value", "query")
        loc = (location, alias)
        if value is None or value == []:
            if field.field_info.is_required():
                errors.append({"type": "missing", "loc": loc, "msg": "Field required", "input": None})
            else:
                values[field.name] = field.get_default()
            continue
        validated, field_errors = field.validate(value, values, loc=loc)
        if field_errors:
            errors.extend(field_errors)
        else:
            values[field.name] = validated
    return values, errors


async def request_body_to_args(body_fields, received_body, embed_body_fields):
    assert body_fields, "request_body_to_args() should be called with fields"
    values, errors = {}, []
    single = len(body_fields) == 1 and not embed_body_fields
    for field in body_fields:
        loc = ("body",) if single else ("body", field.alias)
        value = received_body if single else (received_body or {}).get(field.alias)
        if value is None:
            if field.field_info.is_required():
                errors.append({"type": "missing", "loc": loc, "msg": "Field required", "input": None})
            else:
                values[field.name] = field.get_default()
            continue
        validated, field_errors = field.validate(value, values, loc=loc)
        if field_errors:
            errors.extend(field_errors)
        else:
            values[field.name] = validated
    return values, errors


async def _call_dependency(call, values, stack):
    if inspect.isasyncgenfunction(call):
        return await stack.enter_async_context(asynccontextmanager(call)(**values))
    if inspect.isgeneratorfunction(call):
        from fastrapi._helpers import contextmanager_in_threadpool

        return await stack.enter_async_context(contextmanager_in_threadpool(contextmanager(call)(**values)))
    if inspect.iscoroutinefunction(call) or inspect.iscoroutinefunction(getattr(call, "__call__", None)):
        return await call(**values)
    return await run_in_threadpool(call, **values)


async def solve_dependencies(
    *,
    request,
    dependant,
    body=PydanticUndefined,
    background_tasks=None,
    response=None,
    dependency_overrides_provider=None,
    dependency_cache=None,
    async_exit_stack,
    embed_body_fields,
    _uses_scopes_cache=None,
):
    fastrapi = _fastrapi()
    values, errors = {}, []
    if response is None:
        response = StarletteResponse()
        del response.headers["content-length"]
        response.status_code = None
    cache = {} if dependency_cache is None else dependency_cache
    overrides = getattr(dependency_overrides_provider, "dependency_overrides", None) or {}
    for sub in dependant.dependencies:
        call = overrides.get(sub.call, sub.call)
        used = sub
        if call is not sub.call:
            used = get_dependant(
                path=sub.path,
                call=call,
                name=sub.name,
                own_oauth_scopes=sub.own_oauth_scopes,
                parent_oauth_scopes=sub.parent_oauth_scopes,
                scope=sub.scope,
            )
        solved = await solve_dependencies(
            request=request,
            dependant=used,
            body=body,
            background_tasks=background_tasks,
            response=response,
            dependency_overrides_provider=dependency_overrides_provider,
            dependency_cache=cache,
            async_exit_stack=async_exit_stack,
            embed_body_fields=embed_body_fields,
        )
        background_tasks = solved.background_tasks
        if solved.errors:
            errors.extend(solved.errors)
            continue
        key = used.cache_key
        if sub.use_cache and key in cache:
            value = cache[key]
        else:
            value = await _call_dependency(call, solved.values, async_exit_stack)
            cache[key] = value
        if sub.name is not None:
            values[sub.name] = value
    sources = (
        (dependant.path_params, request.path_params),
        (dependant.query_params, request.query_params),
        (dependant.header_params, request.headers),
        (dependant.cookie_params, request.cookies),
    )
    for fields, received in sources:
        param_values, param_errors = request_params_to_args(fields, received)
        values.update(param_values)
        errors.extend(param_errors)
    if dependant.body_params:
        body_values, body_errors = await request_body_to_args(
            dependant.body_params, None if body is PydanticUndefined else body, embed_body_fields
        )
        values.update(body_values)
        errors.extend(body_errors)
    if dependant.request_param_name and isinstance(request, Request):
        values[dependant.request_param_name] = request
    if dependant.websocket_param_name and isinstance(request, WebSocket):
        values[dependant.websocket_param_name] = request
    if dependant.http_connection_param_name:
        values[dependant.http_connection_param_name] = request
    if dependant.background_tasks_param_name:
        background_tasks = background_tasks if background_tasks is not None else fastrapi.BackgroundTasks()
        values[dependant.background_tasks_param_name] = background_tasks
    if dependant.response_param_name:
        values[dependant.response_param_name] = response
    if dependant.security_scopes_param_name:
        values[dependant.security_scopes_param_name] = fastrapi.security.SecurityScopes(scopes=dependant.oauth_scopes)
    return SolvedDependency(values, errors, background_tasks, response, cache)


async def serialize_response(
    *,
    field=None,
    response_content,
    include=None,
    exclude=None,
    by_alias=True,
    exclude_unset=False,
    exclude_defaults=False,
    exclude_none=False,
    is_coroutine=True,
    endpoint_ctx=None,
    dump_json=False,
):
    from fastrapi.encoders import jsonable_encoder
    from fastrapi.exceptions import ResponseValidationError

    if field is None:
        return jsonable_encoder(response_content)
    if is_coroutine:
        value, errors = field.validate(response_content, {}, loc=("response",))
    else:
        value, errors = await run_in_threadpool(field.validate, response_content, {}, loc=("response",))
    if errors:
        raise ResponseValidationError(errors=errors, body=response_content)
    serializer = field.serialize_json if dump_json else field.serialize
    return serializer(
        value,
        include=include,
        exclude=exclude,
        by_alias=by_alias,
        exclude_unset=exclude_unset,
        exclude_defaults=exclude_defaults,
        exclude_none=exclude_none,
    )


async def run_endpoint_function(*, dependant, values, is_coroutine):
    assert dependant.call is not None, "dependant.call must be a function"
    if is_coroutine:
        return await dependant.call(**values)
    return await run_in_threadpool(dependant.call, **values)


def get_request_handler(
    dependant,
    body_field=None,
    status_code=None,
    response_class=None,
    response_field=None,
    response_model_include=None,
    response_model_exclude=None,
    response_model_by_alias=True,
    response_model_exclude_unset=False,
    response_model_exclude_defaults=False,
    response_model_exclude_none=False,
    dependency_overrides_provider=None,
    embed_body_fields=False,
    **_options,
):
    from contextlib import AsyncExitStack

    from fastrapi.exceptions import RequestValidationError
    from fastrapi.responses import JSONResponse

    response_class = getattr(response_class, "value", response_class) or JSONResponse
    is_coroutine = inspect.iscoroutinefunction(dependant.call)

    async def app(request):
        body = PydanticUndefined
        if body_field is not None:
            raw = await request.body()
            if raw:
                form = isinstance(body_field.field_info, _fastrapi().params.Form)
                body = await request.form() if form else await request.json()
        async with AsyncExitStack() as stack:
            solved = await solve_dependencies(
                request=request,
                dependant=dependant,
                body=body,
                dependency_overrides_provider=dependency_overrides_provider,
                async_exit_stack=stack,
                embed_body_fields=embed_body_fields,
            )
            if solved.errors:
                raise RequestValidationError(solved.errors, body=None if body is PydanticUndefined else body)
            content = await run_endpoint_function(dependant=dependant, values=solved.values, is_coroutine=is_coroutine)
            if isinstance(content, (StarletteResponse, _fastrapi().Response)):
                return content
            serialized = await serialize_response(
                field=response_field,
                response_content=content,
                include=response_model_include,
                exclude=response_model_exclude,
                by_alias=response_model_by_alias,
                exclude_unset=response_model_exclude_unset,
                exclude_defaults=response_model_exclude_defaults,
                exclude_none=response_model_exclude_none,
                is_coroutine=is_coroutine,
            )
            arguments = {"background": solved.background_tasks}
            code = solved.response.status_code or status_code
            if code is not None:
                arguments["status_code"] = code
            response = response_class(serialized, **arguments)
            response.headers.raw.extend(solved.response.headers.raw)
            return response

    return app


def get_fields_from_routes(routes):
    fields = []
    for route in routes:
        if not getattr(route, "include_in_schema", False) or getattr(route, "dependant", None) is None:
            continue
        if route.body_field is not None:
            fields.append(route.body_field)
        if route.response_field is not None:
            fields.append(route.response_field)
        if route.callbacks:
            fields.extend(get_fields_from_routes(route.callbacks))
        fields.extend(get_flat_params(route.dependant))
    return fields


def get_openapi_operation_request_body(*, body_field, model_name_map=None, field_mapping=None, separate_input_output_schemas=True):
    from fastrapi.openapi.constants import REF_TEMPLATE

    if body_field is None:
        return None
    schema = (field_mapping or {}).get((body_field, "validation"))
    if schema is None:
        schema = body_field._type_adapter.json_schema(ref_template=REF_TEMPLATE)
    media_type = getattr(body_field.field_info, "media_type", None) or "application/json"
    request_body = {"content": {media_type: {"schema": schema}}}
    if body_field.field_info.is_required():
        request_body["required"] = True
    return request_body


__all__ = [name for name in dir() if not name.startswith("_") and name not in {"Any", "Callable", "Literal", "Optional", "Sequence", "get_origin"}]
