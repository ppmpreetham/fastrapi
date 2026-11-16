//! Documents the success and validation error responses of an operation like FastAPI's
//! `get_openapi_path`: the response class sets the media type and default status, generator
//! endpoints stream JSON Lines or server-sent events, and the 422 response is added last.

use super::document::MediaType;
use super::registry::SchemaRegistry;
use super::schema::schema_title;
use fastrapi_http::responses::PyHTMLResponse;
use fastrapi_http::responses::PyJSONResponse;
use fastrapi_http::responses::PyPlainTextResponse;
use fastrapi_http::sse::PyEventSourceResponse;
use fastrapi_http::sse::PyServerSentEvent;
use fastrapi_routing::params::params_module;
use fastrapi_routing::route::RouteHandler;
use indexmap::IndexMap;
use pyo3::PyTypeInfo;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyType;
use simd_json::json;
use simd_json::owned::Object;
use simd_json::owned::Value as JsonValue;

const JSON_MEDIA_TYPE: &str = "application/json";

/// The responses that make FastAPI skip its own 422 response.
const VALIDATION_ERROR_COVERED_BY: [&str; 3] = ["422", "4XX", "default"];

/// What a route's response class contributes to the documentation.
pub(crate) struct ResponseClass {
    /// The class's `media_type`. Without one, the success response has no content.
    pub(crate) media_type: Option<String>,
    /// A `JSONResponse` subclass documents the response model, other classes a string.
    json: bool,
    /// The default of the class's `status_code` argument, used when the route sets none.
    pub(crate) status: u16,
}

impl ResponseClass {
    pub(crate) fn of(py: Python<'_>, class: Option<&Py<PyAny>>) -> Self {
        let Some(class) = class.map(|class| class.bind(py)) else {
            return Self {
                media_type: Some(JSON_MEDIA_TYPE.to_owned()),
                json: true,
                status: 200,
            };
        };
        Self {
            media_type: class_media_type(class),
            json: is_subclass::<PyJSONResponse>(class),
            status: default_status(class).unwrap_or(200),
        }
    }
}

/// The class's `media_type` attribute. The native classes keep it per instance, so their
/// documented default is used instead.
fn class_media_type(class: &Bound<'_, PyAny>) -> Option<String> {
    let media_type = class.getattr(intern!(class.py(), "media_type")).ok()?;
    if media_type.is_none() {
        return None;
    }
    media_type
        .extract::<String>()
        .ok()
        .or_else(|| native_media_type(class).map(str::to_owned))
}

fn native_media_type(class: &Bound<'_, PyAny>) -> Option<&'static str> {
    if is_subclass::<PyJSONResponse>(class) {
        Some(JSON_MEDIA_TYPE)
    } else if is_subclass::<PyHTMLResponse>(class) {
        Some("text/html")
    } else if is_subclass::<PyPlainTextResponse>(class) {
        Some("text/plain")
    } else if is_subclass::<PyEventSourceResponse>(class) {
        Some("text/event-stream")
    } else {
        None
    }
}

/// The default of the class's `status_code` argument, read from its signature like FastAPI.
fn default_status(class: &Bound<'_, PyAny>) -> Option<u16> {
    let py = class.py();
    py.import(intern!(py, "inspect"))
        .ok()?
        .call_method1(intern!(py, "signature"), (class,))
        .ok()?
        .getattr(intern!(py, "parameters"))
        .ok()?
        .get_item(intern!(py, "status_code"))
        .ok()?
        .getattr(intern!(py, "default"))
        .ok()?
        .extract()
        .ok()
}

fn is_subclass<T: PyTypeInfo>(class: &Bound<'_, PyAny>) -> bool {
    class
        .cast::<PyType>()
        .is_ok_and(|class| class.is_subclass_of::<T>().unwrap_or(false))
}

#[derive(Clone, Copy)]
enum StreamKind {
    JsonLines,
    ServerSentEvents,
}

impl StreamKind {
    /// Generator endpoints stream JSON Lines with the default response class and server-sent
    /// events with an `EventSourceResponse`.
    fn of(py: Python<'_>, handler: &RouteHandler) -> Option<Self> {
        handler.response.stream_factory.as_ref()?;
        match &handler.response.response_class {
            None => Some(Self::JsonLines),
            Some(class) if is_subclass::<PyEventSourceResponse>(class.bind(py)) => {
                Some(Self::ServerSentEvents)
            }
            Some(_) => None,
        }
    }
}

/// The content of the success response.
pub(crate) fn success_content(
    py: Python<'_>,
    handler: &RouteHandler,
    response_class: &ResponseClass,
    operation_id: &str,
    registry: &mut SchemaRegistry,
) -> Option<IndexMap<String, MediaType>> {
    let (media_type, media) = if let Some(kind) = StreamKind::of(py, handler) {
        let item = stream_item_schema(py, handler, operation_id, registry);
        let (media_type, item_schema) = match kind {
            StreamKind::JsonLines => ("application/jsonl", item.unwrap_or_else(|| json!({}))),
            StreamKind::ServerSentEvents => ("text/event-stream", sse_event_schema(item)),
        };
        let media = MediaType {
            item_schema: Some(item_schema),
            ..MediaType::default()
        };
        (media_type.to_owned(), media)
    } else {
        let media_type = response_class.media_type.clone()?;
        let schema = if response_class.json {
            handler
                .response
                .response_model
                .as_ref()
                .filter(|model| !model.bind(py).is_none())
                .map_or_else(
                    || json!({}),
                    |model| {
                        registry.field(
                            model.bind(py),
                            "serialization",
                            Some(schema_title(&format!("Response_{operation_id}"))),
                        )
                    },
                )
        } else {
            json!({ "type": "string" })
        };
        let media = MediaType {
            schema: Some(schema),
            ..MediaType::default()
        };
        (media_type, media)
    };
    Some(IndexMap::from([(media_type, media)]))
}

/// The schema of the items a generator endpoint yields, from its return annotation.
/// `ServerSentEvent` items are a transport wrapper, so they have no item schema.
fn stream_item_schema(
    py: Python<'_>,
    handler: &RouteHandler,
    operation_id: &str,
    registry: &mut SchemaRegistry,
) -> Option<JsonValue> {
    let params = params_module(py).ok()?;
    let annotation = params
        .call_method1(
            intern!(py, "get_typed_return_annotation"),
            (handler.execution.func.bind(py),),
        )
        .ok()?;
    let item = params
        .call_method1(intern!(py, "get_stream_item_type"), (annotation,))
        .ok()
        .filter(|item| !item.is_none() && !is_subclass::<PyServerSentEvent>(item))?;
    Some(registry.field(
        &item,
        "serialization",
        Some(schema_title(&format!("StreamItem_{operation_id}"))),
    ))
}

/// FastAPI's `_SSE_EVENT_SCHEMA`. With an item schema, `data` is required and holds the item as JSON.
fn sse_event_schema(item_schema: Option<JsonValue>) -> JsonValue {
    let event = |data: JsonValue| {
        json!({
            "type": "object",
            "properties": {
                "data": data,
                "event": { "type": "string" },
                "id": { "type": "string" },
                "retry": { "type": "integer", "minimum": 0 }
            }
        })
    };
    let Some(item_schema) = item_schema else {
        return event(json!({ "type": "string" }));
    };
    let mut schema = event(json!({
        "type": "string",
        "contentMediaType": JSON_MEDIA_TYPE,
        "contentSchema": item_schema
    }));
    if let JsonValue::Object(object) = &mut schema {
        object.insert("required".to_owned(), json!(["data"]));
    }
    schema
}

/// Adds FastAPI's 422 response and its schemas, unless the route's responses already cover it.
pub(crate) fn add_validation_error(
    responses: &mut Object,
    schemas: &mut IndexMap<String, JsonValue>,
) {
    if VALIDATION_ERROR_COVERED_BY
        .iter()
        .any(|status| responses.contains_key(*status))
    {
        return;
    }
    responses.insert(
        "422".to_owned(),
        json!({
            "description": "Validation Error",
            "content": {
                JSON_MEDIA_TYPE: {
                    "schema": { "$ref": "#/components/schemas/HTTPValidationError" }
                }
            }
        }),
    );
    schemas
        .entry("HTTPValidationError".to_owned())
        .or_insert_with(|| {
            json!({
                "properties": {
                    "detail": {
                        "items": { "$ref": "#/components/schemas/ValidationError" },
                        "type": "array",
                        "title": "Detail"
                    }
                },
                "type": "object",
                "title": "HTTPValidationError"
            })
        });
    schemas
        .entry("ValidationError".to_owned())
        .or_insert_with(|| {
            json!({
                "properties": {
                    "loc": {
                        "items": { "anyOf": [ { "type": "string" }, { "type": "integer" } ] },
                        "type": "array",
                        "title": "Location"
                    },
                    "msg": { "type": "string", "title": "Message" },
                    "type": { "type": "string", "title": "Error Type" },
                    "input": { "title": "Input" },
                    "ctx": { "type": "object", "title": "Context" }
                },
                "type": "object",
                "required": ["loc", "msg", "type"],
                "title": "ValidationError"
            })
        });
}
