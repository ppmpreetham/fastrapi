import inspect
import typing
import warnings
from collections import deque
from collections.abc import AsyncGenerator, AsyncIterable, AsyncIterator, Generator, Iterable, Iterator, Mapping, Sequence
from copy import copy, deepcopy
from dataclasses import is_dataclass
from enum import Enum
from functools import partial
from types import NoneType, UnionType
from typing import Annotated, Any, Union, get_args, get_origin

from pydantic import AliasChoices, BaseModel, BeforeValidator, Field, Json, Tag, TypeAdapter
from pydantic.fields import FieldInfo
from pydantic_core import PydanticUndefined, ValidationError

from fastrapi.exceptions import FastAPIDeprecationWarning

Undefined = PydanticUndefined
RequiredParam = PydanticUndefined


class _UnsetType:
    def __repr__(self) -> str:
        return "Unset"


Unset = _UnsetType()


class ParamTypes(Enum):
    query = "query"
    header = "header"
    path = "path"
    cookie = "cookie"


def _init_field(
    self,
    default: Any = Undefined,
    *,
    default_factory=Unset,
    annotation=None,
    alias=None,
    alias_priority=Unset,
    validation_alias=None,
    serialization_alias=None,
    title=None,
    description=None,
    gt=None,
    ge=None,
    lt=None,
    le=None,
    min_length=None,
    max_length=None,
    pattern=None,
    regex=None,
    discriminator=None,
    strict=Unset,
    validate_default=Unset,
    union_mode=Unset,
    fail_fast=Unset,
    multiple_of=Unset,
    allow_inf_nan=Unset,
    max_digits=Unset,
    decimal_places=Unset,
    examples=None,
    example=Unset,
    openapi_examples=None,
    deprecated=None,
    include_in_schema=True,
    json_schema_extra=None,
    **unexpected: Any,
) -> None:
    if unexpected:
        raise TypeError(f"{type(self).__name__}() got an unexpected keyword argument {next(iter(unexpected))!r}")
    if example is not Unset:
        warnings.warn(
            "`example` has been deprecated, please use `examples` instead",
            category=FastAPIDeprecationWarning,
            stacklevel=5,
        )
    self.example = None if example is Unset else example
    self.include_in_schema = include_in_schema
    self.openapi_examples = openapi_examples
    kwargs = dict(
        default=default,
        default_factory=default_factory,
        alias=alias,
        title=title,
        description=description,
        gt=gt,
        ge=ge,
        lt=lt,
        le=le,
        min_length=min_length,
        max_length=max_length,
        discriminator=discriminator,
        validate_default=validate_default,
        union_mode=union_mode,
        fail_fast=fail_fast,
        multiple_of=multiple_of,
        allow_inf_nan=allow_inf_nan,
        max_digits=max_digits,
        decimal_places=decimal_places,
    )
    if examples is not None:
        kwargs["examples"] = examples
    if regex is not None:
        warnings.warn(
            "`regex` has been deprecated, please use `pattern` instead",
            category=FastAPIDeprecationWarning,
            stacklevel=5,
        )
    kwargs["deprecated"] = deprecated
    if serialization_alias in (Unset, None) and isinstance(alias, str):
        serialization_alias = alias
    if validation_alias in (Unset, None):
        validation_alias = alias
    kwargs.update(
        annotation=annotation,
        alias_priority=alias_priority,
        validation_alias=validation_alias,
        serialization_alias=serialization_alias,
        strict=strict,
        json_schema_extra=json_schema_extra,
        pattern=pattern or regex,
    )
    FieldInfo.__init__(self, **{k: v for k, v in kwargs.items() if v is not Unset})


def _repr(self) -> str:
    return f"{self.__class__.__name__}({self.default})"


class Param(FieldInfo):
    in_: ParamTypes
    __init__ = _init_field
    __repr__ = _repr


class Path(Param):
    in_ = ParamTypes.path

    def __init__(self, default: Any = ..., **kwargs: Any) -> None:
        assert default is ..., "Path parameters cannot have a default value"
        self.in_ = self.in_
        super().__init__(default, **kwargs)


class Query(Param):
    in_ = ParamTypes.query


class Header(Param):
    in_ = ParamTypes.header

    def __init__(self, default: Any = Undefined, *, convert_underscores: bool = True, **kwargs: Any) -> None:
        self.convert_underscores = convert_underscores
        super().__init__(default, **kwargs)


class Cookie(Param):
    in_ = ParamTypes.cookie


class Body(FieldInfo):
    __repr__ = _repr

    def __init__(
        self,
        default: Any = Undefined,
        *,
        embed: bool | None = None,
        media_type: str = "application/json",
        **kwargs: Any,
    ) -> None:
        self.embed = embed
        self.media_type = media_type
        _init_field(self, default, **kwargs)


class Form(Body):
    def __init__(self, default: Any = Undefined, *, media_type: str = "application/x-www-form-urlencoded", **kwargs: Any) -> None:
        super().__init__(default, media_type=media_type, **kwargs)


class File(Form):
    def __init__(self, default: Any = Undefined, *, media_type: str = "multipart/form-data", **kwargs: Any) -> None:
        super().__init__(default, media_type=media_type, **kwargs)


def _marker_signature(default: Any = Undefined, after: str = "default_factory", **extra: Any) -> inspect.Signature:
    params = [
        param
        for param in inspect.signature(_init_field).parameters.values()
        if param.name not in ("self", "annotation") and param.kind is not inspect.Parameter.VAR_KEYWORD
    ]
    params[0] = params[0].replace(default=default)
    index = next(i for i, param in enumerate(params) if param.name == after) + 1
    added = [inspect.Parameter(name, inspect.Parameter.KEYWORD_ONLY, default=value) for name, value in extra.items()]
    return inspect.Signature(params[:index] + added + params[index:])


Query.__signature__ = Cookie.__signature__ = _marker_signature()
Path.__signature__ = _marker_signature(...)
Header.__signature__ = _marker_signature(after="serialization_alias", convert_underscores=True)
Body.__signature__ = _marker_signature(embed=None, media_type="application/json")
Form.__signature__ = _marker_signature(media_type="application/x-www-form-urlencoded")
File.__signature__ = _marker_signature(media_type="multipart/form-data")


SEQUENCE_TYPES = (list, set, frozenset, tuple, deque, Sequence)
IMMUTABLE_DEFAULTS = (NoneType, bool, int, float, complex, str, bytes, Enum)
FAST_KINDS = {str: "str", int: "int", float: "float", bool: "bool"}
SOURCES = {ParamTypes.path: "path", ParamTypes.query: "query", ParamTypes.header: "header", ParamTypes.cookie: "cookie"}


def lenient_issubclass(cls: Any, class_or_tuple: Any) -> bool:
    try:
        return isinstance(cls, type) and issubclass(cls, class_or_tuple)
    except TypeError:
        return False


def _is_union(origin: Any) -> bool:
    return origin is Union or origin is UnionType


def _annotation_is_sequence(annotation: Any) -> bool:
    return not lenient_issubclass(annotation, (str, bytes)) and lenient_issubclass(annotation, SEQUENCE_TYPES)


def is_sequence(annotation: Any) -> bool:
    origin = get_origin(annotation)
    if origin is Annotated:
        return is_sequence(get_args(annotation)[0])
    if _is_union(origin):
        return any(is_sequence(arg) for arg in get_args(annotation))
    return _annotation_is_sequence(annotation) or _annotation_is_sequence(origin)


def _annotation_is_complex(annotation: Any, upload: tuple) -> bool:
    return (
        lenient_issubclass(annotation, (BaseModel, Mapping, *upload))
        or _annotation_is_sequence(annotation)
        or is_dataclass(annotation)
    )


def is_complex(annotation: Any, upload: tuple) -> bool:
    origin = get_origin(annotation)
    if _is_union(origin):
        return any(is_complex(arg, upload) for arg in get_args(annotation))
    if origin is Annotated:
        return is_complex(get_args(annotation)[0], upload)
    return (
        _annotation_is_complex(annotation, upload)
        or _annotation_is_complex(origin, upload)
        or hasattr(origin, "__pydantic_core_schema__")
        or hasattr(origin, "__get_pydantic_core_schema__")
    )


def is_scalar(annotation: Any, upload: tuple) -> bool:
    return annotation is Ellipsis or not is_complex(annotation, upload)


def is_scalar_sequence(annotation: Any, upload: tuple) -> bool:
    origin = get_origin(annotation)
    if origin is Annotated:
        return is_scalar_sequence(get_args(annotation)[0], upload)
    if _is_union(origin):
        found = False
        for arg in get_args(annotation):
            if is_scalar_sequence(arg, upload):
                found = True
            elif not is_scalar(arg, upload):
                return False
        return found
    return is_sequence(annotation) and all(is_scalar(arg, upload) for arg in get_args(annotation))


def is_type(annotation: Any, types: tuple) -> bool:
    if lenient_issubclass(annotation, types):
        return True
    if _is_union(get_origin(annotation)):
        return any(lenient_issubclass(arg, types) for arg in get_args(annotation))
    return False


def is_type_sequence(annotation: Any, types: tuple) -> bool:
    if _is_union(get_origin(annotation)):
        return any(is_type_sequence(arg, types) for arg in get_args(annotation))
    return is_sequence(annotation) and all(is_type(arg, types) for arg in get_args(annotation))


def param_model(field_info: FieldInfo) -> type[BaseModel] | None:
    annotation = field_info.annotation
    unwraps = isinstance(field_info, Param) or (isinstance(field_info, Form) and not field_info.embed)
    if unwraps and _is_union(get_origin(annotation)):
        args = get_args(annotation)
        if len(args) == 2 and NoneType in args:
            annotation = next(arg for arg in args if arg is not NoneType)
    return annotation if lenient_issubclass(annotation, BaseModel) and not _is_json(field_info) else None


def form_union_models(field_info: FieldInfo) -> list[type[BaseModel]]:
    annotation = field_info.annotation
    if not isinstance(field_info, Form) or field_info.embed or _is_json(field_info) or not _is_union(get_origin(annotation)):
        return []
    models = []
    for arg in get_args(annotation):
        if arg is NoneType:
            continue
        model = get_args(arg)[0] if get_origin(arg) is Annotated else arg
        if not lenient_issubclass(model, BaseModel):
            return []
        models.append(model)
    return models if len(models) > 1 else []


def form_union_annotation(annotation: Any) -> Any:
    choices = []
    for choice in get_args(annotation):
        if choice is NoneType:
            choices.append(choice)
            continue
        model, *metadata = get_args(choice) if get_origin(choice) is Annotated else (choice,)
        tag = next((item for item in reversed(metadata) if isinstance(item, Tag)), Tag(model.__name__))
        choices.append(Annotated[choice, BeforeValidator(partial(form_union_branch, keys=_model_keys(model, "body", False))), tag])
    return Union[tuple(choices)]


def form_union_branch(value: Any, *, keys: list["ModelKey"]) -> Any:
    if not isinstance(value, dict):
        return value
    branch = {}
    for key in keys:
        if key.lookup not in value:
            continue
        items = value[key.lookup]
        items = items if isinstance(items, list) else [items]
        if key.sequence:
            branch[key.alias] = items
        elif items[-1] != "":
            branch[key.alias] = items[-1]
    return branch


def model_presence(field_info: FieldInfo, models: list[type[BaseModel]]) -> str | None:
    if not models or NoneType not in get_args(field_info.annotation):
        return None
    return "any" if any(model.model_config.get("extra") in ("allow", "forbid") for model in models) else "own"


def copy_field_info(field_info: FieldInfo, annotation: Any) -> FieldInfo:
    merged = type(field_info).from_annotation(annotation)
    new = copy(field_info)
    for name, value in merged.asdict()["attributes"].items():
        setattr(new, name, value)
    new._attributes_set = merged._attributes_set.copy()
    new.metadata = merged.metadata
    new.annotation = merged.annotation
    return new


def type_adapter(field_info: FieldInfo) -> TypeAdapter:
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        fields = field_info.asdict()
        return TypeAdapter(Annotated[(fields["annotation"], *fields["metadata"], Field(**fields["attributes"]))])


def validation_alias(field_info: FieldInfo, name: str) -> str:
    alias = field_info.validation_alias
    if isinstance(alias, str) and alias:
        return alias
    return field_info.alias if field_info.alias is not None else name


def param_aliases(info: FieldInfo, name: str, config: Mapping[str, Any], convert_underscores: bool = False) -> list[tuple[str, str]]:
    alias = validation_alias(info, name)
    request_alias = alias.replace("_", "-") if convert_underscores and alias == name else alias
    choices = info.validation_alias
    aliases = [(alias, request_alias)]
    if isinstance(choices, AliasChoices):
        if not all(isinstance(choice, str) for choice in choices.choices):
            return aliases
        aliases = [(choice, choice) for choice in choices.choices]
    if config.get("validate_by_alias") is False or config.get("validate_by_name", config.get("populate_by_name", False)):
        request_name = name.replace("_", "-") if convert_underscores else name
        if config.get("validate_by_alias") is False:
            return [(name, request_name)]
        if name not in [alias for alias, _ in aliases]:
            aliases.append((name, request_name))
    return aliases


def form_schema_aliases(model: type[BaseModel]) -> list[tuple[str, list[str]]] | None:
    aliases = []
    for name, info in model.model_fields.items():
        choices = info.validation_alias
        schema_name = choices.choices[0] if isinstance(choices, AliasChoices) else validation_alias(info, name)
        names = [alias for alias, _ in param_aliases(info, name, model.model_config)]
        if isinstance(schema_name, str) and (names[0] != schema_name or (len(names) > 1 and info.is_required())):
            aliases.append((schema_name, names))
    return aliases or None


def alias_choices(field_info: FieldInfo) -> list[str] | None:
    alias = field_info.validation_alias
    if isinstance(field_info, Path) or not isinstance(alias, AliasChoices):
        return None
    return list(alias.choices) if all(isinstance(choice, str) for choice in alias.choices) else None


EMPTY_DEFAULTS = {list: "list", dict: "dict", set: "set"}
CONSTRAINT_NAMES = {"Gt": "gt", "Ge": "ge", "Lt": "lt", "Le": "le", "MinLen": "min_length", "MaxLen": "max_length"}
NUMERIC_CONSTRAINTS = {"gt", "ge", "lt", "le"}
TEXT_CONSTRAINTS = {"min_length", "max_length", "pattern"}
EXACT_FLOAT = 2**53


def empty_default(field_info: FieldInfo) -> str | None:
    if field_info.is_required() or field_info.default_factory is not None or field_info.validate_default:
        return None
    default = field_info.default
    kind = EMPTY_DEFAULTS.get(type(default))
    return kind if kind is not None and not default else None


def validated_default(field_info: FieldInfo, validate):
    return lambda: validate(deepcopy(field_info.get_default(call_default_factory=True)))


def default_maker(field_info: FieldInfo):
    if field_info.is_required() or empty_default(field_info) is not None:
        return None
    if field_info.default_factory is not None:
        return lambda: deepcopy(field_info.get_default(call_default_factory=True))
    if isinstance(field_info.default, IMMUTABLE_DEFAULTS):
        return None
    return partial(deepcopy, field_info.default)


def _scalar_kind(annotation: Any) -> str | None:
    if _is_union(get_origin(annotation)):
        args = [arg for arg in get_args(annotation) if arg is not NoneType]
        annotation = args[0] if len(args) == 1 else None
    return FAST_KINDS.get(annotation) if type(annotation) is type else None


def _constraint(item: Any) -> tuple[str, Any] | None:
    name = CONSTRAINT_NAMES.get(type(item).__name__)
    if name is not None:
        return name, getattr(item, name)
    values = vars(item) if type(item).__name__ == "_PydanticGeneralMetadata" else {}
    if set(values) == {"pattern"} and isinstance(values["pattern"], str):
        return "pattern", values["pattern"]
    return None


def fast_kind(field_info: FieldInfo, annotation: Any) -> str | None:
    kind = _scalar_kind(annotation)
    if kind is None:
        return None
    allowed = NUMERIC_CONSTRAINTS if kind in ("int", "float") else TEXT_CONSTRAINTS if kind == "str" else set()
    for item in field_info.metadata:
        constraint = _constraint(item)
        if constraint is None or constraint[0] not in allowed:
            return None
        name, value = constraint
        if name in NUMERIC_CONSTRAINTS and not (type(value) in (int, float) and abs(value) < EXACT_FLOAT):
            return None
    return kind


def fast_list_kind(field_info: FieldInfo, annotation: Any) -> str | None:
    if field_info.metadata:
        return None
    if _is_union(get_origin(annotation)):
        args = [arg for arg in get_args(annotation) if arg is not NoneType]
        annotation = args[0] if len(args) == 1 else None
    if get_origin(annotation) is not list:
        return None
    args = get_args(annotation)
    return _scalar_kind(args[0]) if len(args) == 1 and get_origin(args[0]) is not Annotated else None


def constraints(field_info: FieldInfo) -> dict[str, Any]:
    found = {}
    for item in field_info.metadata:
        for key in ("gt", "ge", "lt", "le", "min_length", "max_length", "pattern"):
            value = getattr(item, key, None)
            if value is not None:
                found[key] = value
    return found


class ModelKey:
    __slots__ = ("lookup", "alias", "sequence")

    def __init__(self, lookup: str, alias: str, info: FieldInfo) -> None:
        self.lookup = lookup
        self.alias = alias
        self.sequence = is_sequence(info.annotation) and not _is_json(info)


class FieldSpec:
    __slots__ = (
        "name",
        "alias",
        "lookup",
        "source",
        "field_info",
        "annotation",
        "required",
        "default",
        "make_default",
        "sequence",
        "model",
        "model_keys",
        "model_class",
        "model_presence",
        "model_union",
        "model_schema_aliases",
        "alias_choices",
        "loc_renames",
        "fast",
        "validate",
        "validate_json",
        "embed",
        "is_form",
        "is_file",
        "media_type",
        "constraints",
        "explicit",
        "read_bytes",
        "fast_list",
        "empty_default",
        "core_schema",
    )


def _model_keys(model: type[BaseModel], source: str, convert_underscores: bool) -> list[ModelKey]:
    return [
        ModelKey(lookup, alias, info)
        for name, info in model.model_fields.items()
        for alias, lookup in param_aliases(
            info,
            name,
            model.model_config,
            source == "header" and getattr(info, "convert_underscores", convert_underscores),
        )
    ]


def _merged_model_keys(models: list[type[BaseModel]], source: str, convert_underscores: bool) -> list[ModelKey]:
    keys = {}
    for model in models:
        for key in _model_keys(model, source, convert_underscores):
            keys.setdefault(key.lookup, key)
    return list(keys.values())


def _with_annotation(field_info: FieldInfo, annotation: Any) -> FieldInfo:
    replaced = copy(field_info)
    replaced.annotation = annotation
    return replaced


def _is_json(field_info: FieldInfo) -> bool:
    return any(type(item) is Json for item in field_info.metadata)


TYPE_ALIAS = getattr(typing, "TypeAliasType", ())


def analyze(param_name: str, parameter: inspect.Parameter, is_path_param: bool, upload: tuple) -> FieldSpec:
    annotation = Undefined if parameter.annotation is inspect.Parameter.empty else parameter.annotation
    value = Undefined if parameter.default is inspect.Parameter.empty else parameter.default
    field_info = None
    use_annotation = Any
    type_annotation = Any
    if isinstance(annotation, TYPE_ALIAS):
        annotation = annotation.__value__
    if annotation is not Undefined:
        use_annotation = annotation
        type_annotation = annotation
    explicit = False
    if get_origin(use_annotation) is Annotated:
        annotated_args = get_args(annotation)
        type_annotation = annotated_args[0]
        markers = [arg for arg in annotated_args[1:] if isinstance(arg, (Param, Body))]
        if markers:
            field_info = copy_field_info(markers[-1], use_annotation)
            explicit = True
            assert field_info.default in (Undefined, RequiredParam), (
                f"`{field_info.__class__.__name__}` default value cannot be set in"
                f" `Annotated` for {param_name!r}. Set the default value with `=` instead."
            )
            if value is not Undefined:
                assert not is_path_param, "Path parameters cannot have default values"
                field_info.default = value
            else:
                field_info.default = RequiredParam
    if isinstance(value, FieldInfo):
        assert field_info is None, (
            "Cannot specify FastAPI annotations in `Annotated` and default value"
            f" together for {param_name!r}"
        )
        field_info = value
        field_info.annotation = type_annotation
        explicit = True
    if field_info is None:
        default = value if value is not Undefined else RequiredParam
        if is_path_param:
            field_info = Path(annotation=use_annotation)
        elif is_type(type_annotation, upload) or is_type_sequence(type_annotation, upload):
            field_info = File(annotation=use_annotation, default=default)
        elif not is_scalar(type_annotation, upload):
            field_info = Body(annotation=use_annotation, default=default)
        else:
            field_info = Query(annotation=use_annotation, default=default)
        if get_origin(use_annotation) is Annotated:
            field_info = copy_field_info(field_info, use_annotation)
            field_info.default = default
            if is_path_param:
                field_info.default_factory = None
    if is_path_param:
        assert isinstance(field_info, Path), f"Cannot use `{field_info.__class__.__name__}` for path param {param_name!r}"
    elif isinstance(field_info, Param) and getattr(field_info, "in_", None) is None:
        field_info.in_ = ParamTypes.query
    if not field_info.alias and getattr(field_info, "convert_underscores", None):
        alias = param_name.replace("_", "-")
    else:
        alias = field_info.alias or param_name
    field_info.alias = alias
    if isinstance(field_info, Path):
        assert is_scalar(field_info.annotation, upload), "Path params must be of one of the supported types"
    elif isinstance(field_info, Query):
        assert (
            is_scalar(field_info.annotation, upload)
            or is_scalar_sequence(field_info.annotation, upload)
            or param_model(field_info) is not None
        ), f"Query parameter {param_name!r} must be one of the supported types"
    spec = FieldSpec()
    spec.name = param_name
    spec.alias_choices = alias_choices(field_info)
    spec.alias = spec.alias_choices[0] if spec.alias_choices else validation_alias(field_info, param_name)
    spec.lookup = spec.alias
    spec.field_info = field_info
    spec.annotation = type_annotation
    spec.explicit = explicit
    spec.source = SOURCES[field_info.in_] if isinstance(field_info, Param) else "body"
    spec.required = field_info.is_required()
    spec.default = None if spec.required else field_info.get_default(call_default_factory=False)
    spec.make_default = default_maker(field_info)
    spec.sequence = is_sequence(field_info.annotation) and not _is_json(field_info)
    model = param_model(field_info)
    union_models = form_union_models(field_info)
    models = union_models or ([model] if model is not None else [])
    spec.model = bool(models)
    spec.model_union = bool(union_models)
    spec.model_class = Union[tuple(union_models)] if union_models else model
    spec.model_schema_aliases = form_schema_aliases(model) if model is not None and isinstance(field_info, Form) else None
    spec.model_presence = model_presence(field_info, models)
    spec.model_keys = (
        _merged_model_keys(models, spec.source, getattr(field_info, "convert_underscores", True))
        if spec.model and (spec.source != "body" or isinstance(field_info, Form))
        else None
    )
    spec.loc_renames = (
        {key.alias: key.lookup for key in spec.model_keys if key.alias != key.lookup} or None
        if spec.model_keys and spec.source == "header" and model.model_config.get("loc_by_alias", True)
        else None
    )
    adapter = type_adapter(_with_annotation(field_info, form_union_annotation(field_info.annotation)) if union_models else field_info)
    spec.validate = partial(adapter.validate_python, from_attributes=True)
    spec.validate_json = adapter.validate_json
    if field_info.validate_default and not spec.required:
        spec.make_default = validated_default(field_info, spec.validate)
    spec.core_schema = adapter.core_schema
    spec.fast = None if spec.sequence else fast_kind(field_info, field_info.annotation)
    spec.fast_list = fast_list_kind(field_info, field_info.annotation) if spec.sequence else None
    spec.empty_default = empty_default(field_info)
    spec.embed = bool(getattr(field_info, "embed", None))
    spec.is_form = isinstance(field_info, Form)
    spec.is_file = isinstance(field_info, File)
    spec.read_bytes = spec.is_file and (
        is_type(field_info.annotation, (bytes,)) or is_type_sequence(field_info.annotation, (bytes,))
    )
    spec.media_type = getattr(field_info, "media_type", None)
    spec.constraints = constraints(field_info)
    return spec


def should_embed(specs: list[FieldSpec]) -> bool:
    if len({spec.name for spec in specs}) > 1:
        return True
    first = specs[0]
    return first.embed or (
        first.is_form and not first.model
    )


def errors_with_loc(exc: ValidationError, loc: tuple, renames: dict[str, str] | None = None) -> list[dict[str, Any]]:
    errors = [{**error, "loc": loc + error.get("loc", ())} for error in exc.errors(include_url=False)]
    if renames:
        for error in errors:
            error_loc = error["loc"]
            if len(error_loc) > 1 and error_loc[1] in renames:
                error["loc"] = (error_loc[0], renames[error_loc[1]], *error_loc[2:])
    return errors


def json_error(raw: bytes) -> tuple[list[dict[str, Any]], Any]:
    import json

    try:
        return [], json.loads(raw)
    except json.JSONDecodeError as exc:
        return [
            {
                "type": "json_invalid",
                "loc": ("body", exc.pos),
                "msg": "JSON decode error",
                "input": {},
                "ctx": {"error": exc.msg},
            }
        ], exc.doc


def _signature(call):
    try:
        return inspect.signature(call, eval_str=True)
    except NameError:
        import sys

        if sys.version_info >= (3, 14):
            from annotationlib import Format

            return inspect.signature(call, annotation_format=Format.FORWARDREF)
        return inspect.signature(call)


def get_typed_annotation(annotation: Any, globalns: dict[str, Any]) -> Any:
    if isinstance(annotation, str):
        from pydantic._internal._typing_extra import try_eval_type

        annotation = try_eval_type(typing.ForwardRef(annotation), globalns, globalns)[0]
        if annotation is NoneType:
            return None
    return annotation


def get_typed_signature(call) -> inspect.Signature:
    signature = _signature(call)
    globalns = getattr(inspect.unwrap(call), "__globals__", {})
    return inspect.Signature(
        [
            inspect.Parameter(
                name=param.name,
                kind=param.kind,
                default=param.default,
                annotation=get_typed_annotation(param.annotation, globalns),
            )
            for param in signature.parameters.values()
        ]
    )


def get_typed_return_annotation(call) -> Any:
    annotation = _signature(call).return_annotation
    if annotation is inspect.Signature.empty:
        return None
    return get_typed_annotation(annotation, getattr(inspect.unwrap(call), "__globals__", {}))


def get_validation_alias(field) -> str:
    return getattr(field, "validation_alias", None) or field.alias


_STREAM_ORIGINS = {AsyncIterable, AsyncIterator, AsyncGenerator, Iterable, Iterator, Generator}


def get_stream_item_type(annotation: Any) -> Any | None:
    origin = get_origin(annotation)
    if origin is not None and origin in _STREAM_ORIGINS:
        return (get_args(annotation) or (Any,))[0]
    return None


multipart_not_installed_error = (
    'Form data requires "python-multipart" to be installed. \n'
    'You can install "python-multipart" with: \n\n'
    "pip install python-multipart\n"
)
multipart_incorrect_install_error = (
    'Form data requires "python-multipart" to be installed. '
    'It seems you installed "multipart" instead. \n'
    'You can remove "multipart" with: \n\n'
    "pip uninstall multipart\n\n"
    'And then install "python-multipart" with: \n\n'
    "pip install python-multipart\n"
)


def ensure_multipart_is_installed() -> None:
    import logging

    logger = logging.getLogger("fastapi")
    try:
        from python_multipart import __version__

        assert __version__ > "0.0.12"
    except (ImportError, AssertionError):
        try:
            from multipart import __version__

            assert __version__
            try:
                from multipart.multipart import parse_options_header

                assert parse_options_header
            except ImportError:
                logger.error(multipart_incorrect_install_error)
                raise RuntimeError(multipart_incorrect_install_error) from None
        except ImportError:
            logger.error(multipart_not_installed_error)
            raise RuntimeError(multipart_not_installed_error) from None


class ParamDetails:
    __slots__ = ("type_annotation", "depends", "field")

    def __init__(self, type_annotation: Any, depends: Any, field: Any) -> None:
        self.type_annotation = type_annotation
        self.depends = depends
        self.field = field


def _is_depends(value: Any) -> bool:
    return hasattr(value, "dependency") and hasattr(value, "use_cache")


def _is_injected(annotation: Any) -> bool:
    import fastrapi
    import fastrapi.security
    from starlette.background import BackgroundTasks
    from starlette.requests import HTTPConnection
    from starlette.responses import Response

    injected = (
        HTTPConnection,
        Response,
        BackgroundTasks,
        fastrapi.Request,
        fastrapi.WebSocket,
        fastrapi.Response,
        fastrapi.BackgroundTasks,
        fastrapi.security.SecurityScopes,
    )
    return inspect.isclass(annotation) and issubclass(annotation, injected)


def analyze_param(*, param_name: str, annotation: Any, value: Any, path_param_names: set) -> ParamDetails:
    from fastrapi import UploadFile
    from fastrapi._helpers import ModelField

    is_path_param = param_name in path_param_names

    annotated = get_origin(annotation) is Annotated
    type_annotation = get_args(annotation)[0] if annotated else annotation
    markers = get_args(annotation)[1:] if annotated else ()
    depends = value if _is_depends(value) else next((marker for marker in reversed(markers) if _is_depends(marker)), None)
    if depends is not None:
        return ParamDetails(type_annotation, depends, None)
    if _is_injected(type_annotation):
        return ParamDetails(type_annotation, None, None)
    parameter = inspect.Parameter(param_name, inspect.Parameter.POSITIONAL_OR_KEYWORD, default=value, annotation=annotation)
    spec = analyze(param_name, parameter, is_path_param, (UploadFile,))
    return ParamDetails(spec.annotation, None, ModelField(field_info=spec.field_info, name=param_name))
