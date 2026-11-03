import re
import warnings
from typing import Any

REF_PREFIX = "#/components/schemas/"

validation_error_definition = {
    "title": "ValidationError",
    "type": "object",
    "properties": {
        "loc": {
            "title": "Location",
            "type": "array",
            "items": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
        },
        "msg": {"title": "Message", "type": "string"},
        "type": {"title": "Error Type", "type": "string"},
        "input": {"title": "Input"},
        "ctx": {"title": "Context", "type": "object"},
    },
    "required": ["loc", "msg", "type"],
}

validation_error_response_definition = {
    "title": "HTTPValidationError",
    "type": "object",
    "properties": {
        "detail": {
            "title": "Detail",
            "type": "array",
            "items": {"$ref": REF_PREFIX + "ValidationError"},
        }
    },
}

status_code_ranges: dict[str, str] = {
    "1XX": "Information",
    "2XX": "Success",
    "3XX": "Redirection",
    "4XX": "Client Error",
    "5XX": "Server Error",
    "DEFAULT": "Default Response",
}


def _path_format(route: Any) -> str:
    return getattr(route, "path_format", None) or re.sub(r"\{([^}:]+)(:[^}]*)?\}", r"{\1}", route.path)


def generate_operation_id(*, route: Any, method: str) -> str:
    from fastrapi.exceptions import FastAPIDeprecationWarning

    warnings.warn(
        message="fastapi.openapi.utils.generate_operation_id() was deprecated, "
        "it is not used internally, and will be removed soon",
        category=FastAPIDeprecationWarning,
        stacklevel=2,
    )
    if route.operation_id:
        return route.operation_id
    operation_id = re.sub(r"\W", "_", f"{route.name}{_path_format(route)}")
    return f"{operation_id}_{method.lower()}"


def generate_operation_summary(*, route: Any, method: str) -> str:
    if route.summary:
        return route.summary
    return route.name.replace("_", " ").title()


def get_openapi_operation_metadata(*, route: Any, method: str, operation_ids: set[str]) -> dict[str, Any]:
    operation: dict[str, Any] = {}
    if route.tags:
        operation["tags"] = route.tags
    operation["summary"] = generate_operation_summary(route=route, method=method)
    if route.description:
        operation["description"] = route.description
    operation_id = route.operation_id or getattr(route, "unique_id", None)
    if not operation_id:
        operation_id = re.sub(r"\W", "_", f"{route.name}{_path_format(route)}")
        operation_id = f"{operation_id}_{method.lower()}"
    if operation_id in operation_ids:
        endpoint_name = getattr(route.endpoint, "__name__", "<unnamed_endpoint>")
        message = f"Duplicate Operation ID {operation_id} for function {endpoint_name}"
        file_name = getattr(route.endpoint, "__globals__", {}).get("__file__")
        if file_name:
            message += f" at {file_name}"
        warnings.warn(message, stacklevel=1)
    operation_ids.add(operation_id)
    operation["operationId"] = operation_id
    if route.deprecated:
        operation["deprecated"] = route.deprecated
    return operation


def get_openapi_path(
    *,
    route: Any,
    operation_ids: set[str] | None = None,
    model_name_map: Any = None,
    field_mapping: Any = None,
    separate_input_output_schemas: bool = True,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    from fastrapi.openapi.utils import get_openapi

    spec = get_openapi(
        title="",
        version="",
        routes=[route],
        separate_input_output_schemas=separate_input_output_schemas,
    )
    components = spec.get("components", {})
    path = spec.get("paths", {}).get(_path_format(route), {})
    if operation_ids is not None:
        for operation in path.values():
            if isinstance(operation, dict) and "operationId" in operation:
                operation_ids.add(operation["operationId"])
    return path, components.get("securitySchemes", {}), components.get("schemas", {})
