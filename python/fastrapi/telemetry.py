import atexit
import contextvars
import os
import threading
from collections.abc import Callable, Iterator, MutableMapping, Sequence
from contextlib import AbstractContextManager, contextmanager, nullcontext
from dataclasses import dataclass, field
from time import perf_counter, time_ns
from typing import Any, TypedDict
from urllib.parse import parse_qsl, urlencode, urlsplit

from opentelemetry import _logs, metrics, propagate, trace
from opentelemetry import context as otel_context
from opentelemetry._logs import Logger, LoggerProvider, SeverityNumber
from opentelemetry.context import Context
from opentelemetry.metrics import Histogram, MeterProvider, UpDownCounter
from opentelemetry.propagators.textmap import Getter
from opentelemetry.trace import Span, SpanKind, StatusCode, Tracer, TracerProvider
from starlette.datastructures import Headers
from starlette.exceptions import HTTPException, WebSocketException
from starlette.requests import Request
from starlette.websockets import WebSocket, WebSocketDisconnect

import fastrapi
from fastrapi.exceptions import FastAPIError, RequestValidationError, WebSocketRequestValidationError
from fastrapi.logger import logger

_HTTP_METHODS = frozenset({"CONNECT", "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "QUERY", "TRACE"})
_DURATION_BUCKETS = (0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0)
_SCHEMA_URL = "https://opentelemetry.io/schemas/1.44.0"
_SENSITIVE_QUERY_PARAMETERS = frozenset({"X-Amz-Signature", "X-Amz-Credential", "X-Amz-Security-Token", "sig", "X-Goog-Signature"})
_DEFERRED_PROVIDERS = frozenset(
    {
        ("opentelemetry.trace", "ProxyTracerProvider"),
        ("opentelemetry.metrics._internal", "_ProxyMeterProvider"),
        ("opentelemetry._logs._internal", "ProxyLoggerProvider"),
    }
)


class TelemetryConfig(TypedDict, total=False):
    """Optional settings for `FastAPI(telemetry={...})`.

    Omitted settings keep their defaults. FastAPI preserves existing providers
    and their exporters. Environment setup can add exporters when `auto_configure`
    is `True`. Providers supplied by the application are never shut down by FastAPI.
    """

    tracer_provider: TracerProvider | None
    meter_provider: MeterProvider | None
    logger_provider: LoggerProvider | None
    tracing: bool
    metrics: bool
    logs: bool
    operation_spans: bool
    auto_configure: bool
    exclude: Callable[[MutableMapping[str, Any]], bool] | None


@dataclass(kw_only=True)
class TelemetryData:
    """Local request or connection data for synchronous OpenTelemetry processors.

    FastAPI populates these fields as it handles the request or connection.
    Integrations should treat the data as read-only and apply their capture and
    redaction settings before exporting any of it.
    """

    request: Request | None = None
    websocket: WebSocket | None = None
    body: Any = None
    values: dict[str, Any] | None = None
    errors: Sequence[Any] | None = None


@dataclass(kw_only=True)
class _RequestTelemetry:
    span: Span | None
    span_method: str
    tracer: Tracer | None = None
    logger: Logger | None = None
    route: str | None = None
    status_code: int | None = None
    data: TelemetryData | None = field(default_factory=TelemetryData)
    _mount_prefix: str = ""
    _exceptions: list[BaseException] = field(default_factory=list, repr=False)


_REQUEST_TELEMETRY_KEY = otel_context.create_key("fastapi.request")
_NO_OPERATION = nullcontext()


def _get_request_telemetry(context: Context | None = None) -> _RequestTelemetry | None:
    value = otel_context.get_value(_REQUEST_TELEMETRY_KEY, context)
    return value if isinstance(value, _RequestTelemetry) else None


def get_telemetry_data(context: Context | None = None) -> TelemetryData | None:
    """Read local FastAPI data from an OpenTelemetry context.

    Defaults to the current context. Log processors can pass `record.context`.
    Read the data synchronously while FastAPI handles the request or connection.
    After it finishes, this returns `None`, including for retained contexts.
    This data is not automatically added to exported spans or logs.
    """
    request_telemetry = _get_request_telemetry(context)
    return request_telemetry.data if request_telemetry is not None else None


def _validation_failed(exc: RequestValidationError | WebSocketRequestValidationError) -> None:
    request_telemetry = _get_request_telemetry()
    if request_telemetry is None:
        return
    if request_telemetry.data is not None:
        if isinstance(exc, RequestValidationError):
            request_telemetry.data.body = exc.body
        request_telemetry.data.errors = exc.errors()
    if request_telemetry.logger is not None:
        attributes: dict[str, Any] = {"fastapi.validation.error_count": len(exc.errors())}
        if request_telemetry.route is not None:
            attributes["http.route"] = request_telemetry.route
        request_telemetry.logger.emit(
            event_name="fastapi.validation.failed",
            timestamp=time_ns(),
            severity_number=SeverityNumber.WARN,
            severity_text="WARN",
            body="Request validation failed",
            attributes=attributes,
        )


def _exception_type(exc: BaseException) -> str:
    cls = type(exc)
    return f"{cls.__module__}.{cls.__qualname__}" if cls.__module__ != "builtins" else cls.__qualname__


def _normal_websocket_disconnect(exc: BaseException) -> bool:
    return isinstance(exc, WebSocketDisconnect) and exc.code in (1000, 1001)


def _operation(*, name: str, function: Callable[..., Any] | None = None) -> AbstractContextManager[None]:
    request_telemetry = _get_request_telemetry()
    if request_telemetry is None or request_telemetry.tracer is None:
        return _NO_OPERATION
    return _traced_operation(request_telemetry=request_telemetry, name=name, function=function)


@contextmanager
def _traced_operation(*, request_telemetry: _RequestTelemetry, name: str, function: Callable[..., Any] | None) -> Iterator[None]:
    assert request_telemetry.tracer is not None
    with request_telemetry.tracer.start_as_current_span(
        f"fastapi.{name}",
        attributes=_operation_attributes(function),
        record_exception=False,
        set_status_on_exception=False,
    ) as span:
        try:
            yield
        except Exception as exc:
            expected = (HTTPException, RequestValidationError, WebSocketException, WebSocketRequestValidationError)
            if name == "background_task" or (not isinstance(exc, expected) and not _normal_websocket_disconnect(exc)):
                span.set_attribute("error.type", _exception_type(exc))
                span.set_status(StatusCode.ERROR)
            raise


def _operation_attributes(function: Callable[..., Any] | None) -> dict[str, str]:
    if function is None:
        return {}
    module = getattr(function, "__module__", type(function).__module__)
    qualname = getattr(function, "__qualname__", type(function).__qualname__)
    return {"code.function.name": f"{module}.{qualname}"}


def start_operation(context: contextvars.Context, name: str, function: Callable[..., Any] | None) -> tuple[Span, Context] | None:
    """Opens a `fastapi.{name}` span and makes it current in the request's context."""

    def start() -> tuple[Span, Context] | None:
        request_telemetry = _get_request_telemetry()
        if request_telemetry is None or request_telemetry.tracer is None:
            return None
        previous = otel_context.get_current()
        span = request_telemetry.tracer.start_span(f"fastapi.{name}", attributes=_operation_attributes(function))
        otel_context.attach(trace.set_span_in_context(span))
        return span, previous

    return context.run(start)


def finish_operation(context: contextvars.Context, name: str, operation: tuple[Span, Context], error: BaseException | None) -> None:
    span, previous = operation
    if name == "dependencies" and isinstance(error, (RequestValidationError, WebSocketRequestValidationError)):
        context.run(_validation_failed, error)
    expected = (HTTPException, RequestValidationError, WebSocketException, WebSocketRequestValidationError)
    if isinstance(error, Exception) and (name == "background_task" or (not isinstance(error, expected) and not _normal_websocket_disconnect(error))):
        span.set_attribute("error.type", _exception_type(error))
        span.set_status(StatusCode.ERROR)
    span.end()
    context.run(otel_context.attach, previous)


def _unconfigured(provider: Any) -> bool:
    cls = type(provider)
    return (cls.__module__, cls.__name__) in _DEFERRED_PROVIDERS


class _HeadersGetter(Getter[Headers]):
    def get(self, carrier: Headers, key: str) -> list[str] | None:
        values = carrier.getlist(key)
        if not values:
            return None
        if key.lower() == "baggage":
            return [",".join(values)]
        return values

    def keys(self, carrier: Headers) -> list[str]:
        return carrier.keys()


_HEADERS_GETTER = _HeadersGetter()


def _exception(*, scope: MutableMapping[str, Any], request_telemetry: _RequestTelemetry, exc: BaseException) -> None:
    if scope["type"] == "websocket" and _normal_websocket_disconnect(exc):
        return
    seen = request_telemetry._exceptions
    if any(previous is exc for previous in seen):
        return
    seen.append(exc)
    if request_telemetry.logger is not None and isinstance(exc, Exception):
        websocket = scope["type"] == "websocket"
        request_telemetry.logger.emit(
            exception=exc,
            event_name="fastapi.websocket.exception" if websocket else "http.server.request.exception",
            timestamp=time_ns(),
            severity_number=SeverityNumber.ERROR,
            severity_text="ERROR",
            body="Unhandled exception in FastAPI WebSocket connection" if websocket else "Unhandled exception in FastAPI request",
            attributes={"http.route": request_telemetry.route} if request_telemetry.route else None,
        )


def _server_attributes(scope: MutableMapping[str, Any]) -> dict[str, Any]:
    authority = Headers(scope=scope).get("host")
    if authority is not None:
        try:
            url = urlsplit("//" + authority)
            address, port = url.hostname, url.port
        except ValueError:
            return {}
        if url.username is not None or url.path or url.query or url.fragment:
            return {}
        if port is None:
            port = {"http": 80, "https": 443, "ws": 80, "wss": 443}.get(scope.get("scheme", "http"))
    else:
        address, port = scope.get("server") or (None, None)
    if address is None:
        return {}
    attributes: dict[str, Any] = {"server.address": address}
    if port is not None:
        attributes["server.port"] = port
    return attributes


def _attach_value(key: str, value: Any) -> None:
    otel_context.attach(otel_context.set_value(key, value))


def _attach_span(span: Span) -> None:
    otel_context.attach(trace.set_span_in_context(span))


class _Exchange:
    """One request or connection: the context its handlers run in and the work left when it ends."""

    __slots__ = ("context", "_finish")

    def __init__(self, context: contextvars.Context, finish: Callable[..., None] | None) -> None:
        self.context = context
        self._finish = finish

    def finish(self, status_code: int | None = None, error: BaseException | None = None) -> None:
        if self._finish is not None:
            finish, self._finish = self._finish, None
            finish(status_code, error)


class NativeTelemetry:
    def __init__(self, config: TelemetryConfig) -> None:
        self.config = config
        known_methods = os.getenv("OTEL_INSTRUMENTATION_HTTP_KNOWN_METHODS", "").strip()
        self._known_methods = (
            frozenset(method.strip() for method in known_methods.split(",") if method.strip()) if known_methods else _HTTP_METHODS
        )
        self._provider: metrics.MeterProvider | None = None
        self._duration: Histogram | None = None
        self._active: UpDownCounter | None = None
        self._tracer_provider: trace.TracerProvider | None = None
        self._tracer: trace.Tracer | None = None

    def configure(self) -> None:
        try:
            _configure_from_environment(self.config)
        except Exception as exc:
            logger.warning("FastAPI automatic telemetry configuration failed: %s", exc)

    def enabled(self) -> bool:
        config = self.config
        return bool(
            (config["tracing"] and (config["tracer_provider"] is not None or not _unconfigured(trace.get_tracer_provider())))
            or (config["metrics"] and (config["meter_provider"] is not None or not _unconfigured(metrics.get_meter_provider())))
            or (config["logs"] and (config["logger_provider"] is not None or not _unconfigured(_logs.get_logger_provider())))
        )

    def _instruments(self) -> tuple[Histogram, UpDownCounter]:
        provider = self.config["meter_provider"] or metrics.get_meter_provider()
        if provider is not self._provider:
            meter = provider.get_meter("fastapi", fastrapi.__version__, schema_url=_SCHEMA_URL)
            self._duration = meter.create_histogram(
                "http.server.request.duration",
                unit="s",
                description="Duration of HTTP server requests.",
                explicit_bucket_boundaries_advisory=_DURATION_BUCKETS,
            )
            self._active = meter.create_up_down_counter(
                "http.server.active_requests",
                unit="{request}",
                description="Number of active HTTP server requests.",
            )
            self._provider = provider
        assert self._duration is not None and self._active is not None
        return self._duration, self._active

    def _get_tracer(self, *, provider: trace.TracerProvider) -> trace.Tracer:
        if provider is not self._tracer_provider:
            self._tracer = provider.get_tracer("fastapi", fastrapi.__version__, schema_url=_SCHEMA_URL)
            self._tracer_provider = provider
        assert self._tracer is not None
        return self._tracer

    def start(self, scope: MutableMapping[str, Any], route: str | None = None) -> _Exchange | None:
        config = self.config
        context = contextvars.copy_context()
        if config["exclude"] is not None and config["exclude"](scope):
            context.run(_attach_value, _REQUEST_TELEMETRY_KEY, None)
            return _Exchange(context, None)
        tracing = config["tracing"]
        logging = config["logs"]
        if logging:
            logger_provider = config["logger_provider"] or _logs.get_logger_provider()
            logging = not (
                (config["logger_provider"] is None and _unconfigured(logger_provider))
                or isinstance(logger_provider, _logs.NoOpLoggerProvider)
            )
        is_websocket = scope["type"] == "websocket"
        metering = config["metrics"] and not is_websocket
        if tracing:
            provider = config["tracer_provider"] or trace.get_tracer_provider()
            tracing = not ((config["tracer_provider"] is None and _unconfigured(provider)) or isinstance(provider, trace.NoOpTracerProvider))
        if metering:
            meter_provider = config["meter_provider"] or metrics.get_meter_provider()
            metering = not (
                (config["meter_provider"] is None and _unconfigured(meter_provider))
                or isinstance(meter_provider, metrics.NoOpMeterProvider)
            )
        if not tracing and not metering and not logging:
            return None
        original_method = scope.get("method", "")
        method = original_method if original_method in self._known_methods else "_OTHER"
        span_method = "WS" if is_websocket else "HTTP" if method == "_OTHER" else method
        attributes: dict[str, Any] = {"url.scheme": scope.get("scheme", "ws" if is_websocket else "http")}
        if is_websocket:
            attributes["network.protocol.name"] = "websocket"
        else:
            attributes["http.request.method"] = method
            if scope.get("http_version"):
                attributes["network.protocol.version"] = scope["http_version"]
        duration, active = self._instruments() if metering else (None, None)
        active_attributes = {k: v for k, v in attributes.items() if k != "network.protocol.version"}
        if active is not None:
            active.add(1, active_attributes)
        started = perf_counter()
        span = None
        tracer = None
        if tracing:
            parent = propagate.extract(Headers(scope=scope), getter=_HEADERS_GETTER)
            context.run(otel_context.attach, parent)
            tracer = self._get_tracer(provider=provider)
            span_attributes = {**attributes, **_server_attributes(scope)}
            if not is_websocket:
                span_attributes["url.path"] = scope["path"]
                if scope.get("query_string"):
                    span_attributes["url.query"] = urlencode(
                        [
                            (key, "REDACTED" if key in _SENSITIVE_QUERY_PARAMETERS else value)
                            for key, value in parse_qsl(scope["query_string"].decode("latin-1"), keep_blank_values=True)
                        ]
                    )
                if original_method != method:
                    span_attributes["http.request.method_original"] = original_method
            span = tracer.start_span(span_method, context=parent, kind=SpanKind.SERVER, attributes=span_attributes)
        request_telemetry = _RequestTelemetry(
            span=span,
            span_method=span_method,
            tracer=tracer if config["operation_spans"] else None,
            logger=_logs.get_logger("fastapi", fastrapi.__version__, config["logger_provider"], schema_url=_SCHEMA_URL) if logging else None,
            _mount_prefix=scope.get("app_root_path", scope.get("root_path", "")).rstrip("/"),
        )
        if route is not None:
            request_telemetry.route = request_telemetry._mount_prefix + route
            if span is not None:
                span.set_attribute("http.route", request_telemetry.route)
                span.update_name(f"{span_method} {request_telemetry.route}")
        context.run(_attach_value, _REQUEST_TELEMETRY_KEY, request_telemetry)
        if span is not None:
            context.run(_attach_span, span)

        def finish(status_code: int | None, error: BaseException | None) -> None:
            if error is not None:
                _exception(scope=scope, request_telemetry=request_telemetry, exc=error)
            request_telemetry.status_code = None if is_websocket else status_code
            if request_telemetry.route is not None:
                attributes["http.route"] = request_telemetry.route
            if request_telemetry.status_code is not None:
                attributes["http.response.status_code"] = request_telemetry.status_code
            failed = error is not None or not is_websocket and (request_telemetry.status_code is None or request_telemetry.status_code >= 500)
            if failed:
                errors = request_telemetry._exceptions
                failure = error or (errors[-1] if errors else None)
                attributes["error.type"] = (
                    _exception_type(failure) if failure else str(request_telemetry.status_code or "incomplete_response")
                )
            if span is not None:
                span.set_attributes(attributes)
                if failed:
                    span.set_status(StatusCode.ERROR)
            if duration is not None:
                context.run(duration.record, max(0, perf_counter() - started), attributes)
            if active is not None:
                active.add(-1, active_attributes)
            if span is not None:
                span.end()
            request_telemetry.data = None
            request_telemetry._exceptions.clear()

        return _Exchange(context, finish)


_lock = threading.RLock()
_owned: list[Any] = []
_configured: list[tuple[str, Any]] = []


def flush() -> None:
    for component in tuple(_owned):
        try:
            component.force_flush()
        except Exception:
            logger.exception("FastAPI telemetry cleanup failed")


def _shutdown() -> None:
    with _lock:
        owned = tuple(_owned)
        _owned.clear()
    for component in owned:
        try:
            component.shutdown()
        except Exception:
            logger.exception("FastAPI telemetry cleanup failed")


atexit.register(_shutdown)


def _export_endpoint(signal: str) -> str | None:
    exporter = (os.getenv(f"OTEL_{signal}_EXPORTER") or "otlp").strip().lower()
    endpoint = os.getenv(f"OTEL_EXPORTER_OTLP_{signal}_ENDPOINT")
    if not endpoint:
        endpoint = os.getenv("OTEL_EXPORTER_OTLP_ENDPOINT")
        if endpoint:
            endpoint = f"{endpoint.removesuffix('/')}/v1/{signal.lower()}"
    if not endpoint or exporter == "none":
        return None
    if exporter != "otlp":
        raise FastAPIError(
            f"FastAPI automatic telemetry supports OTEL_{signal}_EXPORTER=otlp or none. "
            "Configure other exporters explicitly and pass telemetry={'auto_configure': False} to FastAPI()."
        )
    protocol = os.getenv(f"OTEL_EXPORTER_OTLP_{signal}_PROTOCOL") or os.getenv("OTEL_EXPORTER_OTLP_PROTOCOL") or "http/protobuf"
    if protocol != "http/protobuf":
        raise FastAPIError(
            "FastAPI automatic telemetry requires the OTLP http/protobuf protocol. Configure other transports explicitly and pass telemetry={'auto_configure': False} to FastAPI()."
        )
    parsed = urlsplit(endpoint)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise FastAPIError(f"Invalid OTLP {signal.lower()} endpoint. Use an absolute HTTP or HTTPS URL.")
    return endpoint


def _registration_provider(provider: Any) -> Any:
    if (
        type(provider).__module__ == "logfire._internal.metrics"
        and type(provider).__name__ == "ProxyMeterProvider"
        and not hasattr(provider, "add_metric_reader")
    ):
        return provider.provider
    return provider


def _configure_from_environment(config: TelemetryConfig) -> None:
    if not config["auto_configure"] or os.getenv("OTEL_SDK_DISABLED", "").lower() == "true":
        return
    with _lock:
        signals = [
            ("TRACES", config["tracing"], config["tracer_provider"]),
            ("METRICS", config["metrics"], config["meter_provider"]),
            ("LOGS", config["logs"], config["logger_provider"]),
        ]
        requested = [
            (signal, provider, endpoint)
            for signal, enabled, provider in signals
            if enabled and (endpoint := _export_endpoint(signal)) is not None
        ]
        if not requested:
            return
        try:
            from opentelemetry.exporter.otlp.proto.http._log_exporter import OTLPLogExporter
            from opentelemetry.exporter.otlp.proto.http.metric_exporter import OTLPMetricExporter
            from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
            from opentelemetry.sdk._logs import LoggerProvider as SdkLoggerProvider
            from opentelemetry.sdk._logs.export import BatchLogRecordProcessor
            from opentelemetry.sdk.metrics import MeterProvider as SdkMeterProvider
            from opentelemetry.sdk.metrics.export import PeriodicExportingMetricReader
            from opentelemetry.sdk.trace import TracerProvider as SdkTracerProvider
            from opentelemetry.sdk.trace.export import BatchSpanProcessor
        except ImportError as exc:
            raise FastAPIError(
                "Automatic OpenTelemetry export requires fastrapi[opentelemetry]. Install the extra, configure providers yourself, or pass telemetry={'auto_configure': False} to FastAPI()."
            ) from exc
        providers = {
            "TRACES": ("tracer", trace.get_tracer_provider),
            "METRICS": ("meter", metrics.get_meter_provider),
            "LOGS": ("logger", _logs.get_logger_provider),
        }

        def create_component(*, signal: str, endpoint: str) -> Any:
            if signal == "TRACES":
                return BatchSpanProcessor(OTLPSpanExporter(endpoint=endpoint))
            if signal == "METRICS":
                return PeriodicExportingMetricReader(OTLPMetricExporter(endpoint=endpoint))
            return BatchLogRecordProcessor(OTLPLogExporter(endpoint=endpoint))

        for signal, explicit, endpoint in requested:
            name, get_provider = providers[signal]
            provider = _registration_provider(explicit if explicit is not None else get_provider())
            if any(s == signal and p is provider for s, p in _configured):
                continue
            if explicit is None and _unconfigured(provider):
                component = create_component(signal=signal, endpoint=endpoint)
                if signal == "TRACES":
                    provider = SdkTracerProvider(shutdown_on_exit=False)
                    provider.add_span_processor(component)
                    trace.set_tracer_provider(provider)
                elif signal == "METRICS":
                    provider = SdkMeterProvider(metric_readers=[component], shutdown_on_exit=False)
                    metrics.set_meter_provider(provider)
                else:
                    provider = SdkLoggerProvider(shutdown_on_exit=False)
                    provider.add_log_record_processor(component)
                    _logs.set_logger_provider(provider)
                current = get_provider()
                if current is provider:
                    _configured.append((signal, provider))
                    _owned.append(provider)
                    continue
                provider.shutdown()
                provider = _registration_provider(current)
            try:
                if signal == "TRACES":
                    register = provider.add_span_processor
                elif signal == "METRICS":
                    register = provider.add_metric_reader
                else:
                    register = provider.add_log_record_processor
            except AttributeError:
                register = None
            if not callable(register):
                raise FastAPIError(
                    f"The configured OpenTelemetry {name} provider does not support "
                    "adding an OTLP exporter. Configure environment export through "
                    "that provider and pass telemetry={'auto_configure': False} to FastAPI()."
                )
            component = create_component(signal=signal, endpoint=endpoint)
            try:
                register(component)
            except Exception:
                component.shutdown()
                raise
            _configured.append((signal, provider))
            _owned.append(component)
