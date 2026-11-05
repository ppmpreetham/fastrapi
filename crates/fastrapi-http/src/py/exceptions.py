from collections.abc import Mapping, Sequence
from typing import Any, TypedDict

from pydantic import create_model
from starlette.exceptions import HTTPException as StarletteHTTPException


class EndpointContext(TypedDict, total=False):
    function: str
    path: str
    file: str
    line: int


class HTTPException(StarletteHTTPException):
    def __init__(
        self,
        status_code: int,
        detail: Any = None,
        headers: Mapping[str, str] | None = None,
    ) -> None:
        super().__init__(status_code=status_code, detail=detail, headers=headers)


RequestErrorModel = create_model("Request")
WebSocketErrorModel = create_model("WebSocket")


class FastAPIError(RuntimeError):
    pass


class DependencyScopeError(FastAPIError):
    pass


class PydanticV1NotSupportedError(FastAPIError):
    pass


class FastAPIDeprecationWarning(UserWarning):
    pass


class ValidationException(Exception):
    def __init__(
        self,
        errors: Sequence[Any],
        *,
        endpoint_ctx: EndpointContext | None = None,
    ) -> None:
        self._errors = errors
        self.endpoint_ctx = endpoint_ctx
        ctx = endpoint_ctx or {}
        self.endpoint_function = ctx.get("function")
        self.endpoint_path = ctx.get("path")
        self.endpoint_file = ctx.get("file")
        self.endpoint_line = ctx.get("line")

    def errors(self) -> Sequence[Any]:
        return self._errors

    def _format_endpoint_context(self) -> str:
        if not (self.endpoint_file and self.endpoint_line and self.endpoint_function):
            if self.endpoint_path:
                return f"\n  Endpoint: {self.endpoint_path}"
            return ""
        context = f'\n  File "{self.endpoint_file}", line {self.endpoint_line}, in {self.endpoint_function}'
        if self.endpoint_path:
            context += f"\n    {self.endpoint_path}"
        return context

    def __str__(self) -> str:
        message = f"{len(self._errors)} validation error{'s' if len(self._errors) != 1 else ''}:\n"
        for err in self._errors:
            message += f"  {err}\n"
        message += self._format_endpoint_context()
        return message.rstrip()


class RequestValidationError(ValidationException):
    def __init__(
        self,
        errors: Sequence[Any],
        *,
        body: Any = None,
        endpoint_ctx: EndpointContext | None = None,
    ) -> None:
        super().__init__(errors, endpoint_ctx=endpoint_ctx)
        self.body = body


class WebSocketRequestValidationError(ValidationException):
    pass


class ResponseValidationError(ValidationException):
    def __init__(
        self,
        errors: Sequence[Any],
        *,
        body: Any = None,
        endpoint_ctx: EndpointContext | None = None,
    ) -> None:
        super().__init__(errors, endpoint_ctx=endpoint_ctx)
        self.body = body
