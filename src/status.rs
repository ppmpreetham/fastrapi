use pyo3::prelude::*;
use pyo3::types::PyModule;

// HTTP Status Codes
pub const HTTP_100_CONTINUE: u16 = 100;
pub const HTTP_101_SWITCHING_PROTOCOLS: u16 = 101;
pub const HTTP_102_PROCESSING: u16 = 102;
pub const HTTP_103_EARLY_HINTS: u16 = 103;
pub const HTTP_200_OK: u16 = 200;
pub const HTTP_201_CREATED: u16 = 201;
pub const HTTP_202_ACCEPTED: u16 = 202;
pub const HTTP_203_NON_AUTHORITATIVE_INFORMATION: u16 = 203;
pub const HTTP_204_NO_CONTENT: u16 = 204;
pub const HTTP_205_RESET_CONTENT: u16 = 205;
pub const HTTP_206_PARTIAL_CONTENT: u16 = 206;
pub const HTTP_207_MULTI_STATUS: u16 = 207;
pub const HTTP_208_ALREADY_REPORTED: u16 = 208;
pub const HTTP_226_IM_USED: u16 = 226;
pub const HTTP_300_MULTIPLE_CHOICES: u16 = 300;
pub const HTTP_301_MOVED_PERMANENTLY: u16 = 301;
pub const HTTP_302_FOUND: u16 = 302;
pub const HTTP_303_SEE_OTHER: u16 = 303;
pub const HTTP_304_NOT_MODIFIED: u16 = 304;
pub const HTTP_305_USE_PROXY: u16 = 305;
pub const HTTP_306_RESERVED: u16 = 306;
pub const HTTP_307_TEMPORARY_REDIRECT: u16 = 307;
pub const HTTP_308_PERMANENT_REDIRECT: u16 = 308;
pub const HTTP_400_BAD_REQUEST: u16 = 400;
pub const HTTP_401_UNAUTHORIZED: u16 = 401;
pub const HTTP_402_PAYMENT_REQUIRED: u16 = 402;
pub const HTTP_403_FORBIDDEN: u16 = 403;
pub const HTTP_404_NOT_FOUND: u16 = 404;
pub const HTTP_405_METHOD_NOT_ALLOWED: u16 = 405;
pub const HTTP_406_NOT_ACCEPTABLE: u16 = 406;
pub const HTTP_407_PROXY_AUTHENTICATION_REQUIRED: u16 = 407;
pub const HTTP_408_REQUEST_TIMEOUT: u16 = 408;
pub const HTTP_409_CONFLICT: u16 = 409;
pub const HTTP_410_GONE: u16 = 410;
pub const HTTP_411_LENGTH_REQUIRED: u16 = 411;
pub const HTTP_412_PRECONDITION_FAILED: u16 = 412;
pub const HTTP_413_CONTENT_TOO_LARGE: u16 = 413;
pub const HTTP_414_URI_TOO_LONG: u16 = 414;
pub const HTTP_415_UNSUPPORTED_MEDIA_TYPE: u16 = 415;
pub const HTTP_416_RANGE_NOT_SATISFIABLE: u16 = 416;
pub const HTTP_417_EXPECTATION_FAILED: u16 = 417;
pub const HTTP_418_IM_A_TEAPOT: u16 = 418;
pub const HTTP_421_MISDIRECTED_REQUEST: u16 = 421;
pub const HTTP_422_UNPROCESSABLE_CONTENT: u16 = 422;
pub const HTTP_423_LOCKED: u16 = 423;
pub const HTTP_424_FAILED_DEPENDENCY: u16 = 424;
pub const HTTP_425_TOO_EARLY: u16 = 425;
pub const HTTP_426_UPGRADE_REQUIRED: u16 = 426;
pub const HTTP_428_PRECONDITION_REQUIRED: u16 = 428;
pub const HTTP_429_TOO_MANY_REQUESTS: u16 = 429;
pub const HTTP_431_REQUEST_HEADER_FIELDS_TOO_LARGE: u16 = 431;
pub const HTTP_451_UNAVAILABLE_FOR_LEGAL_REASONS: u16 = 451;
pub const HTTP_500_INTERNAL_SERVER_ERROR: u16 = 500;
pub const HTTP_501_NOT_IMPLEMENTED: u16 = 501;
pub const HTTP_502_BAD_GATEWAY: u16 = 502;
pub const HTTP_503_SERVICE_UNAVAILABLE: u16 = 503;
pub const HTTP_504_GATEWAY_TIMEOUT: u16 = 504;
pub const HTTP_505_HTTP_VERSION_NOT_SUPPORTED: u16 = 505;
pub const HTTP_506_VARIANT_ALSO_NEGOTIATES: u16 = 506;
pub const HTTP_507_INSUFFICIENT_STORAGE: u16 = 507;
pub const HTTP_508_LOOP_DETECTED: u16 = 508;
pub const HTTP_510_NOT_EXTENDED: u16 = 510;
pub const HTTP_511_NETWORK_AUTHENTICATION_REQUIRED: u16 = 511;

// WebSocket Codes
pub const WS_1000_NORMAL_CLOSURE: u16 = 1000;
pub const WS_1001_GOING_AWAY: u16 = 1001;
pub const WS_1002_PROTOCOL_ERROR: u16 = 1002;
pub const WS_1003_UNSUPPORTED_DATA: u16 = 1003;
pub const WS_1005_NO_STATUS_RCVD: u16 = 1005;
pub const WS_1006_ABNORMAL_CLOSURE: u16 = 1006;
pub const WS_1007_INVALID_FRAME_PAYLOAD_DATA: u16 = 1007;
pub const WS_1008_POLICY_VIOLATION: u16 = 1008;
pub const WS_1009_MESSAGE_TOO_BIG: u16 = 1009;
pub const WS_1010_MANDATORY_EXT: u16 = 1010;
pub const WS_1011_INTERNAL_ERROR: u16 = 1011;
pub const WS_1012_SERVICE_RESTART: u16 = 1012;
pub const WS_1013_TRY_AGAIN_LATER: u16 = 1013;
pub const WS_1014_BAD_GATEWAY: u16 = 1014;
pub const WS_1015_TLS_HANDSHAKE: u16 = 1015;

/// for the status submodule.
pub fn create_status_submodule(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    let status_module = PyModule::new(py, "status")?;

    // HTTP Codes
    status_module.add("HTTP_100_CONTINUE", HTTP_100_CONTINUE)?;
    status_module.add("HTTP_101_SWITCHING_PROTOCOLS", HTTP_101_SWITCHING_PROTOCOLS)?;
    status_module.add("HTTP_102_PROCESSING", HTTP_102_PROCESSING)?;
    status_module.add("HTTP_103_EARLY_HINTS", HTTP_103_EARLY_HINTS)?;
    status_module.add("HTTP_200_OK", HTTP_200_OK)?;
    status_module.add("HTTP_201_CREATED", HTTP_201_CREATED)?;
    status_module.add("HTTP_202_ACCEPTED", HTTP_202_ACCEPTED)?;
    status_module.add(
        "HTTP_203_NON_AUTHORITATIVE_INFORMATION",
        HTTP_203_NON_AUTHORITATIVE_INFORMATION,
    )?;
    status_module.add("HTTP_204_NO_CONTENT", HTTP_204_NO_CONTENT)?;
    status_module.add("HTTP_205_RESET_CONTENT", HTTP_205_RESET_CONTENT)?;
    status_module.add("HTTP_206_PARTIAL_CONTENT", HTTP_206_PARTIAL_CONTENT)?;
    status_module.add("HTTP_207_MULTI_STATUS", HTTP_207_MULTI_STATUS)?;
    status_module.add("HTTP_208_ALREADY_REPORTED", HTTP_208_ALREADY_REPORTED)?;
    status_module.add("HTTP_226_IM_USED", HTTP_226_IM_USED)?;
    status_module.add("HTTP_300_MULTIPLE_CHOICES", HTTP_300_MULTIPLE_CHOICES)?;
    status_module.add("HTTP_301_MOVED_PERMANENTLY", HTTP_301_MOVED_PERMANENTLY)?;
    status_module.add("HTTP_302_FOUND", HTTP_302_FOUND)?;
    status_module.add("HTTP_303_SEE_OTHER", HTTP_303_SEE_OTHER)?;
    status_module.add("HTTP_304_NOT_MODIFIED", HTTP_304_NOT_MODIFIED)?;
    status_module.add("HTTP_305_USE_PROXY", HTTP_305_USE_PROXY)?;
    status_module.add("HTTP_306_RESERVED", HTTP_306_RESERVED)?;
    status_module.add("HTTP_307_TEMPORARY_REDIRECT", HTTP_307_TEMPORARY_REDIRECT)?;
    status_module.add("HTTP_308_PERMANENT_REDIRECT", HTTP_308_PERMANENT_REDIRECT)?;
    status_module.add("HTTP_400_BAD_REQUEST", HTTP_400_BAD_REQUEST)?;
    status_module.add("HTTP_401_UNAUTHORIZED", HTTP_401_UNAUTHORIZED)?;
    status_module.add("HTTP_402_PAYMENT_REQUIRED", HTTP_402_PAYMENT_REQUIRED)?;
    status_module.add("HTTP_403_FORBIDDEN", HTTP_403_FORBIDDEN)?;
    status_module.add("HTTP_404_NOT_FOUND", HTTP_404_NOT_FOUND)?;
    status_module.add("HTTP_405_METHOD_NOT_ALLOWED", HTTP_405_METHOD_NOT_ALLOWED)?;
    status_module.add("HTTP_406_NOT_ACCEPTABLE", HTTP_406_NOT_ACCEPTABLE)?;
    status_module.add(
        "HTTP_407_PROXY_AUTHENTICATION_REQUIRED",
        HTTP_407_PROXY_AUTHENTICATION_REQUIRED,
    )?;
    status_module.add("HTTP_408_REQUEST_TIMEOUT", HTTP_408_REQUEST_TIMEOUT)?;
    status_module.add("HTTP_409_CONFLICT", HTTP_409_CONFLICT)?;
    status_module.add("HTTP_410_GONE", HTTP_410_GONE)?;
    status_module.add("HTTP_411_LENGTH_REQUIRED", HTTP_411_LENGTH_REQUIRED)?;
    status_module.add("HTTP_412_PRECONDITION_FAILED", HTTP_412_PRECONDITION_FAILED)?;
    status_module.add("HTTP_413_CONTENT_TOO_LARGE", HTTP_413_CONTENT_TOO_LARGE)?;
    status_module.add("HTTP_414_URI_TOO_LONG", HTTP_414_URI_TOO_LONG)?;
    status_module.add(
        "HTTP_415_UNSUPPORTED_MEDIA_TYPE",
        HTTP_415_UNSUPPORTED_MEDIA_TYPE,
    )?;
    status_module.add(
        "HTTP_416_RANGE_NOT_SATISFIABLE",
        HTTP_416_RANGE_NOT_SATISFIABLE,
    )?;
    status_module.add("HTTP_417_EXPECTATION_FAILED", HTTP_417_EXPECTATION_FAILED)?;
    status_module.add("HTTP_418_IM_A_TEAPOT", HTTP_418_IM_A_TEAPOT)?;
    status_module.add("HTTP_421_MISDIRECTED_REQUEST", HTTP_421_MISDIRECTED_REQUEST)?;
    status_module.add(
        "HTTP_422_UNPROCESSABLE_CONTENT",
        HTTP_422_UNPROCESSABLE_CONTENT,
    )?;
    status_module.add("HTTP_423_LOCKED", HTTP_423_LOCKED)?;
    status_module.add("HTTP_424_FAILED_DEPENDENCY", HTTP_424_FAILED_DEPENDENCY)?;
    status_module.add("HTTP_425_TOO_EARLY", HTTP_425_TOO_EARLY)?;
    status_module.add("HTTP_426_UPGRADE_REQUIRED", HTTP_426_UPGRADE_REQUIRED)?;
    status_module.add(
        "HTTP_428_PRECONDITION_REQUIRED",
        HTTP_428_PRECONDITION_REQUIRED,
    )?;
    status_module.add("HTTP_429_TOO_MANY_REQUESTS", HTTP_429_TOO_MANY_REQUESTS)?;
    status_module.add(
        "HTTP_431_REQUEST_HEADER_FIELDS_TOO_LARGE",
        HTTP_431_REQUEST_HEADER_FIELDS_TOO_LARGE,
    )?;
    status_module.add(
        "HTTP_451_UNAVAILABLE_FOR_LEGAL_REASONS",
        HTTP_451_UNAVAILABLE_FOR_LEGAL_REASONS,
    )?;
    status_module.add(
        "HTTP_500_INTERNAL_SERVER_ERROR",
        HTTP_500_INTERNAL_SERVER_ERROR,
    )?;
    status_module.add("HTTP_501_NOT_IMPLEMENTED", HTTP_501_NOT_IMPLEMENTED)?;
    status_module.add("HTTP_502_BAD_GATEWAY", HTTP_502_BAD_GATEWAY)?;
    status_module.add("HTTP_503_SERVICE_UNAVAILABLE", HTTP_503_SERVICE_UNAVAILABLE)?;
    status_module.add("HTTP_504_GATEWAY_TIMEOUT", HTTP_504_GATEWAY_TIMEOUT)?;
    status_module.add(
        "HTTP_505_HTTP_VERSION_NOT_SUPPORTED",
        HTTP_505_HTTP_VERSION_NOT_SUPPORTED,
    )?;
    status_module.add(
        "HTTP_506_VARIANT_ALSO_NEGOTIATES",
        HTTP_506_VARIANT_ALSO_NEGOTIATES,
    )?;
    status_module.add(
        "HTTP_507_INSUFFICIENT_STORAGE",
        HTTP_507_INSUFFICIENT_STORAGE,
    )?;
    status_module.add("HTTP_508_LOOP_DETECTED", HTTP_508_LOOP_DETECTED)?;
    status_module.add("HTTP_510_NOT_EXTENDED", HTTP_510_NOT_EXTENDED)?;
    status_module.add(
        "HTTP_511_NETWORK_AUTHENTICATION_REQUIRED",
        HTTP_511_NETWORK_AUTHENTICATION_REQUIRED,
    )?;

    // webSocket Codes
    status_module.add("WS_1000_NORMAL_CLOSURE", WS_1000_NORMAL_CLOSURE)?;
    status_module.add("WS_1001_GOING_AWAY", WS_1001_GOING_AWAY)?;
    status_module.add("WS_1002_PROTOCOL_ERROR", WS_1002_PROTOCOL_ERROR)?;
    status_module.add("WS_1003_UNSUPPORTED_DATA", WS_1003_UNSUPPORTED_DATA)?;
    status_module.add("WS_1005_NO_STATUS_RCVD", WS_1005_NO_STATUS_RCVD)?;
    status_module.add("WS_1006_ABNORMAL_CLOSURE", WS_1006_ABNORMAL_CLOSURE)?;
    status_module.add(
        "WS_1007_INVALID_FRAME_PAYLOAD_DATA",
        WS_1007_INVALID_FRAME_PAYLOAD_DATA,
    )?;
    status_module.add("WS_1008_POLICY_VIOLATION", WS_1008_POLICY_VIOLATION)?;
    status_module.add("WS_1009_MESSAGE_TOO_BIG", WS_1009_MESSAGE_TOO_BIG)?;
    status_module.add("WS_1010_MANDATORY_EXT", WS_1010_MANDATORY_EXT)?;
    status_module.add("WS_1011_INTERNAL_ERROR", WS_1011_INTERNAL_ERROR)?;
    status_module.add("WS_1012_SERVICE_RESTART", WS_1012_SERVICE_RESTART)?;
    status_module.add("WS_1013_TRY_AGAIN_LATER", WS_1013_TRY_AGAIN_LATER)?;
    status_module.add("WS_1014_BAD_GATEWAY", WS_1014_BAD_GATEWAY)?;
    status_module.add("WS_1015_TLS_HANDSHAKE", WS_1015_TLS_HANDSHAKE)?;

    parent.add_submodule(&status_module)?;

    // Register in sys.modules
    py.import("sys")?
        .getattr("modules")?
        .set_item("fastrapi.status", &status_module)?;

    Ok(())
}
