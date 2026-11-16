import time

import pytest

pytest.importorskip("opentelemetry.sdk")

import fastapi
from fastapi.testclient import TestClient
from opentelemetry import trace
from opentelemetry.sdk._logs import LoggerProvider
from opentelemetry.sdk._logs.export import InMemoryLogExporter, SimpleLogRecordProcessor
from opentelemetry.sdk.metrics import MeterProvider
from opentelemetry.sdk.metrics.export import InMemoryMetricReader
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import SimpleSpanProcessor
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

import fastrapi
import fastrapi.telemetry
from test_security_ws_parity import LiveServer


IGNORED = {"server.port", "network.protocol.version"}


def traced(lib):
    exporter = InMemorySpanExporter()
    tracer_provider = TracerProvider()
    tracer_provider.add_span_processor(SimpleSpanProcessor(exporter))
    reader = InMemoryMetricReader()
    meter_provider = MeterProvider(metric_readers=[reader])
    config = {"tracer_provider": tracer_provider, "meter_provider": meter_provider, "logs": False,
              "exclude": lambda scope: scope["path"] == "/skip"}
    app = lib.FastAPI(telemetry=config) if lib is fastapi else lib.FastrAPI(telemetry=config)
    tracer = tracer_provider.get_tracer("user")

    @app.get("/items/{item_id}")
    def item(item_id: int, q: str = "x"):
        with tracer.start_as_current_span("user-span"):
            return {"item_id": item_id}

    @app.get("/boom")
    def boom():
        raise ValueError("boom")

    @app.get("/skip")
    def skip():
        return "skip"

    client = TestClient(app, raise_server_exceptions=False)
    statuses = [client.get(p).status_code for p in ("/items/3?q=y", "/boom", "/missing", "/skip")]
    client.get("/items/4", headers={"traceparent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"})
    spans = exporter.get_finished_spans()
    server = [s for s in spans if s.kind == trace.SpanKind.SERVER]
    user = [s for s in spans if s.name == "user-span"]
    parented = [u.parent is not None and any(u.parent.span_id == s.context.span_id for s in server) for u in user]
    remote_parent = [format(s.parent.trace_id, "032x") if s.parent else None for s in server][-1]
    summary = [
        (s.name, s.status.status_code.name, {k: v for k, v in sorted(s.attributes.items()) if k not in IGNORED})
        for s in server
    ]
    by_id = {span.context.span_id: span for span in spans}
    tree = sorted(
        (
            span.name,
            span.kind.name,
            span.status.status_code.name,
            by_id[span.parent.span_id].name if span.parent and span.parent.span_id in by_id else None,
            dict(sorted((k, v) for k, v in span.attributes.items() if k.startswith("code.") or k == "error.type")),
        )
        for span in spans
    )
    metric_names = sorted(
        m.name
        for rm in reader.get_metrics_data().resource_metrics
        for sm in rm.scope_metrics
        for m in sm.metrics
    )
    return statuses, summary, parented, remote_parent, metric_names, [repr(t) for t in tree]


def logged(lib):
    logs = InMemoryLogExporter()
    logger_provider = LoggerProvider()
    logger_provider.add_log_record_processor(SimpleLogRecordProcessor(logs))
    spans = InMemorySpanExporter()
    tracer_provider = TracerProvider()
    tracer_provider.add_span_processor(SimpleSpanProcessor(spans))
    config = {"tracer_provider": tracer_provider, "logger_provider": logger_provider, "metrics": False}
    app = lib.FastAPI(telemetry=config) if lib is fastapi else lib.FastrAPI(telemetry=config)

    def dep(token: str):
        return token

    @app.get("/plain/{n}")
    def plain(n: int):
        return n

    @app.get("/deps")
    def deps(q: int, t=lib.Depends(dep)):
        return q

    @app.get("/boom")
    async def boom():
        raise KeyError("x")

    client = TestClient(app, raise_server_exceptions=False)
    statuses = [client.get(p).status_code for p in ("/plain/x", "/deps?q=1", "/boom", "/plain/3")]
    records = []
    for data in logs.get_finished_logs():
        record = data.log_record
        attributes = dict(record.attributes or {})
        attributes.pop("exception.stacktrace", None)
        records.append((record.event_name, record.severity_text, str(record.body), dict(sorted(attributes.items()))))
    span_status = sorted((s.name, s.status.status_code.name, s.attributes.get("error.type")) for s in spans.get_finished_spans())
    return statuses, records, span_status


def test_spans_metrics_and_propagation_match_fastapi():
    assert traced(fastrapi) == traced(fastapi)


def test_log_events_match_fastapi():
    assert logged(fastrapi) == logged(fastapi)


def test_public_api():
    assert {name for name in ("TelemetryConfig", "TelemetryData", "get_telemetry_data") if hasattr(fastrapi.telemetry, name)} == {
        "TelemetryConfig",
        "TelemetryData",
        "get_telemetry_data",
    }
    assert fastrapi.telemetry.get_telemetry_data() is None


def test_native_server_records_operation_spans():
    exporter = InMemorySpanExporter()
    provider = TracerProvider()
    provider.add_span_processor(SimpleSpanProcessor(exporter))
    app = fastrapi.FastrAPI(telemetry={"tracer_provider": provider, "metrics": False, "logs": False})

    @app.get("/items/{item_id}")
    async def item(item_id: int):
        return item_id

    live = LiveServer(app)
    time.sleep(0.2)
    exporter.clear()
    assert live.http("GET", "/items/5") == (200, 5)
    time.sleep(0.2)
    names = sorted(span.name for span in exporter.get_finished_spans())
    assert names == ["GET /items/{item_id}", "fastapi.dependencies", "fastapi.endpoint", "fastapi.serialization"]


def test_disabled_telemetry_keeps_fast_routes():
    app = fastrapi.FastrAPI(telemetry={"tracing": False, "metrics": False, "logs": False})

    @app.get("/")
    def root():
        return "ok"

    assert TestClient(app).get("/").json() == "ok"
