use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule, PyString};
use std::collections::HashSet;

fn register_asyncio_alias(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let asyncio = py.import("asyncio")?;
    m.add("asyncio", &asyncio)?;

    let sys_modules = py
        .import("sys")?
        .getattr("modules")?
        .cast_into::<PyDict>()?;
    sys_modules.set_item("fastrapi.asyncio", asyncio)?;
    Ok(())
}

fn register_submodules_in_sys_modules(m: &Bound<'_, PyModule>) -> PyResult<()> {
    fn walk(
        module: &Bound<'_, PyModule>,
        qualified_name: &str,
        visited: &mut HashSet<usize>,
    ) -> PyResult<()> {
        if !visited.insert(module.as_ptr() as usize) {
            return Ok(());
        }

        let py = module.py();
        let sys_modules = py.import("sys")?.getattr("modules")?;
        let dict = module.dict();

        for (key, value) in dict.iter() {
            let Ok(key) = key.cast::<PyString>() else {
                continue;
            };
            let Ok(value_module) = value.cast::<PyModule>() else {
                continue;
            };

            let child_name = format!("{}.{}", qualified_name, key.to_str()?);
            if !sys_modules.contains(child_name.as_str())? {
                sys_modules.set_item(child_name.as_str(), value_module)?;
            }
            walk(value_module, &child_name, visited)?;
        }

        Ok(())
    }

    walk(m, "fastrapi", &mut HashSet::default())?;
    Ok(())
}

#[pymodule(gil_used = false)]
mod fastrapi {
    use pyo3::prelude::*;
    use pyo3::types::{PyList, PyModule};

    // Top-level exported classes
    #[pymodule_export]
    use crate::engine::config::FastrAPI as FastAPI;

    #[pymodule_export]
    use crate::engine::config::FastrAPI;

    #[pymodule_export]
    use crate::ffi::decorators::PyAPIRouter as APIRouter;

    #[pymodule_export]
    use fastrapi_http::request::PyHTTPConnection as HTTPConnection;

    #[pymodule_export]
    use fastrapi_http::request::PyRequest as Request;

    #[pymodule_export]
    use fastrapi_http::background::PyBackgroundTasks as BackgroundTasks;

    #[pymodule_export]
    use fastrapi_http::upload_file::PyUploadFile as UploadFile;

    #[pymodule_export]
    use fastrapi_http::websocket::PyWebSocket as WebSocket;

    #[pymodule_export]
    use fastrapi_http::exceptions::PyWebSocketException as WebSocketException;

    #[pymodule_export]
    use fastrapi_http::exceptions::PyWebSocketDisconnect as WebSocketDisconnect;

    #[pymodule_export]
    use fastrapi_http::staticfiles::PyStaticFiles as StaticFiles;

    #[pymodule_export]
    use fastrapi_http::exceptions::PyHTTPException as HTTPException;

    #[pymodule_export]
    use fastrapi_routing::params::PyBody as Body;
    #[pymodule_export]
    use fastrapi_routing::params::PyCookie as Cookie;
    #[pymodule_export]
    use fastrapi_routing::params::PyDepends as Depends;
    #[pymodule_export]
    use fastrapi_routing::params::PyFile as File;
    #[pymodule_export]
    use fastrapi_routing::params::PyForm as Form;
    #[pymodule_export]
    use fastrapi_routing::params::PyHeader as Header;
    #[pymodule_export]
    use fastrapi_routing::params::PyPath as Path;
    #[pymodule_export]
    use fastrapi_routing::params::PyQuery as Query;
    #[pymodule_export]
    use fastrapi_routing::params::PySecurity as Security;

    #[pymodule_export]
    use fastrapi_routing::security::APIKeyCookie;
    #[pymodule_export]
    use fastrapi_routing::security::APIKeyHeader;
    #[pymodule_export]
    use fastrapi_routing::security::APIKeyQuery;
    #[pymodule_export]
    use fastrapi_routing::security::HTTPAuthorizationCredentials;
    #[pymodule_export]
    use fastrapi_routing::security::HTTPBasic;
    #[pymodule_export]
    use fastrapi_routing::security::HTTPBasicCredentials;
    #[pymodule_export]
    use fastrapi_routing::security::HTTPBearer;
    #[pymodule_export]
    use fastrapi_routing::security::OAuth2;
    #[pymodule_export]
    use fastrapi_routing::security::OAuth2PasswordBearer;
    #[pymodule_export]
    use fastrapi_routing::security::OAuth2PasswordRequestForm;
    #[pymodule_export]
    use fastrapi_routing::security::OAuth2PasswordRequestFormStrict;
    #[pymodule_export]
    use fastrapi_routing::security::PySecurityScopes as SecurityScopes;

    #[pymodule_export]
    use crate::engine::metrics::PyInstrumentator as Instrumentator;
    use crate::ffi::compat::register_compat_modules;
    use crate::ffi::encoders;
    use crate::ffi::openapi;
    use fastrapi_http::status::create_status_submodule;
    use fastrapi_routing::kernel_validation::kernel_validation_count;
    use fastrapi_routing::pydantic::register_pydantic_integration;

    #[pymodule]
    mod responses {
        use pyo3::prelude::*;

        #[pymodule_export]
        use fastrapi_http::responses::{
            PyFileResponse as FileResponse, PyHTMLResponse as HTMLResponse,
            PyJSONResponse as JSONResponse, PyORJSONResponse as ORJSONResponse,
            PyPlainTextResponse as PlainTextResponse, PyRedirectResponse as RedirectResponse,
            PyResponse, PyStreamingResponse as StreamingResponse, PyUJSONResponse as UJSONResponse,
        };
        #[pymodule_export]
        use fastrapi_http::sse::PyEventSourceResponse as EventSourceResponse;

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            let py = m.py();
            m.add("Response", py.get_type::<PyResponse>())
        }
    }

    #[pymodule]
    mod sse {
        #[pymodule_export]
        use fastrapi_http::sse::PyEventSourceResponse as EventSourceResponse;
        #[pymodule_export]
        use fastrapi_http::sse::PyServerSentEvent as ServerSentEvent;
        use fastrapi_http::sse::register_sse_api;
        use pyo3::prelude::*;

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            register_sse_api(m)
        }
    }

    #[pymodule]
    mod exceptions {
        #[pymodule_export]
        use fastrapi_http::exceptions::{
            PyDependencyScopeError as DependencyScopeError,
            PyFastAPIDeprecationWarning as FastAPIDeprecationWarning,
            PyFastAPIError as FastAPIError, PyHTTPException as HTTPException,
            PyPydanticV1NotSupportedError as PydanticV1NotSupportedError,
            PyRequestValidationError as RequestValidationError,
            PyResponseValidationError as ResponseValidationError,
            PyValidationException as ValidationException,
            PyWebSocketDisconnect as WebSocketDisconnect,
            PyWebSocketException as WebSocketException,
            PyWebSocketRequestValidationError as WebSocketRequestValidationError,
        };
    }

    #[pymodule]
    mod params {
        #[pymodule_export]
        use fastrapi_routing::params::{
            PyBody as Body, PyCookie as Cookie, PyDepends as Depends, PyFile as File,
            PyForm as Form, PyHeader as Header, PyPath as Path, PyQuery as Query,
            PySecurity as Security, Undefined, Unset,
        };
    }

    #[pymodule]
    mod request {
        #[pymodule_export]
        use fastrapi_http::request::{
            PyAppInfo as AppInfo, PyClientInfo as ClientInfo, PyHTTPConnection as HTTPConnection,
            PyRequest as Request,
        };
    }

    #[pymodule]
    mod requests {
        #[pymodule_export]
        use fastrapi_http::request::{
            PyAppInfo as AppInfo, PyClientInfo as ClientInfo, PyHTTPConnection as HTTPConnection,
            PyRequest as Request,
        };
    }

    #[pymodule]
    mod routing {
        use pyo3::prelude::*;

        #[pymodule_export]
        use crate::ffi::decorators::PyAPIRouter as APIRouter;

        use crate::ffi::routing::register;
        #[pymodule_export]
        use crate::ffi::routing::{
            PyAPIRoute as APIRoute, PyAPIWebSocketRoute as APIWebSocketRoute,
            PyRouteContext as RouteContext, iter_route_contexts,
        };

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            register(m)?;
            let py = m.py();
            let routing = py.import("starlette.routing")?;
            for name in ["BaseRoute", "Mount", "WebSocketRoute"] {
                if let Ok(value) = routing.getattr(name) {
                    m.add(name, value)?;
                }
            }
            Ok(())
        }
    }

    #[pymodule]
    mod datastructures {
        use pyo3::prelude::*;

        #[pymodule_export]
        use fastrapi_http::form_data::PyFormData as FormData;
        #[pymodule_export]
        use fastrapi_http::upload_file::PyUploadFile as UploadFile;
        #[pymodule_export]
        use fastrapi_http::url::PyURL as URL;

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            let py = m.py();
            let module = py.import("starlette.datastructures")?;
            for name in [
                "Address",
                "Headers",
                "MutableHeaders",
                "QueryParams",
                "State",
            ] {
                if let Ok(value) = module.getattr(name) {
                    m.add(name, value)?;
                }
            }
            Ok(())
        }
    }

    #[pymodule]
    mod background {
        #[pymodule_export]
        use fastrapi_http::background::PyBackgroundTasks as BackgroundTasks;
    }

    #[pymodule]
    mod security {
        use pyo3::prelude::*;

        use crate::ffi::compat::register_security;
        use fastrapi_routing::security::register_utils_submodule;
        #[pymodule_export]
        use fastrapi_routing::security::{
            APIKeyCookie, APIKeyHeader, APIKeyQuery, HTTPAuthorizationCredentials, HTTPBasic,
            HTTPBasicCredentials, HTTPBearer, HTTPDigest, OAuth2, OAuth2AuthorizationCodeBearer,
            OAuth2PasswordBearer, OAuth2PasswordRequestForm, OAuth2PasswordRequestFormStrict,
            OpenIdConnect, PySecurityScopes as SecurityScopes,
        };

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            register_utils_submodule(m).and_then(|()| register_security(m))
        }

        #[pymodule]
        mod api_key {
            #[pymodule_export]
            use fastrapi_routing::security::{APIKeyCookie, APIKeyHeader, APIKeyQuery};
        }

        #[pymodule]
        mod http {
            #[pymodule_export]
            use fastrapi_routing::security::{
                HTTPAuthorizationCredentials, HTTPBasic, HTTPBasicCredentials, HTTPBearer,
                HTTPDigest,
            };
        }

        #[pymodule]
        mod oauth2 {
            #[pymodule_export]
            use fastrapi_routing::security::{
                OAuth2, OAuth2AuthorizationCodeBearer, OAuth2PasswordBearer,
                OAuth2PasswordRequestForm, OAuth2PasswordRequestFormStrict,
            };
        }

        #[pymodule]
        mod open_id_connect_url {
            #[pymodule_export]
            use fastrapi_routing::security::OpenIdConnect;
        }

        #[pymodule]
        mod base {
            #[pymodule_export]
            use fastrapi_routing::security::{OAuth2, PySecurityScopes as SecurityScopes};
        }
    }

    #[pymodule]
    mod dependencies {
        use crate::ffi::compat::register_dependencies;
        use pyo3::prelude::*;

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            register_dependencies(m)
        }
    }

    #[pymodule]
    mod exception_handlers {
        use crate::ffi::compat::register_exception_handlers;
        use pyo3::prelude::*;

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            register_exception_handlers(m)
        }
    }

    #[pymodule]
    mod staticfiles {
        #[pymodule_export]
        use fastrapi_http::staticfiles::PyStaticFiles as StaticFiles;
    }

    #[pymodule]
    mod templating {
        #[pymodule_export]
        use fastrapi_http::templating::PyJinja2Templates as Jinja2Templates;
    }

    #[pymodule]
    mod middleware {
        use pyo3::prelude::*;

        #[pymodule_export]
        use fastrapi_http::middleware::{
            CORSMiddleware, GZipMiddleware, HTTPSRedirectMiddleware, SessionMiddleware,
            TrustedHostMiddleware,
        };

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            let py = m.py();
            m.add(
                "Middleware",
                py.import("starlette.middleware")?.getattr("Middleware")?,
            )?;
            m.add(
                "BaseHTTPMiddleware",
                py.import("starlette.middleware.base")?
                    .getattr("BaseHTTPMiddleware")?,
            )?;
            m.add(
                "WSGIMiddleware",
                py.import("starlette.middleware.wsgi")?
                    .getattr("WSGIMiddleware")?,
            )
        }

        #[pymodule]
        mod cors {

            #[pymodule_export]
            use fastrapi_http::middleware::CORSMiddleware;
        }
    }

    #[pymodule]
    mod prometheus {
        #[pymodule_export]
        use crate::engine::metrics::PyInstrumentator as Instrumentator;
    }

    #[pymodule]
    mod websocket {
        use fastrapi_http::exceptions::{PyWebSocketDisconnect, PyWebSocketException};
        use pyo3::prelude::*;

        #[pymodule_export]
        use fastrapi_http::websocket::PyWebSocket as WebSocket;

        #[pymodule_init]
        fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
            let py = m.py();
            m.add(
                "WebSocketDisconnect",
                py.get_type::<PyWebSocketDisconnect>(),
            )?;
            m.add("WebSocketException", py.get_type::<PyWebSocketException>())?;
            if let Ok(value) = py
                .import("starlette.websockets")
                .and_then(|module| module.getattr("WebSocketState"))
            {
                m.add("WebSocketState", value)?;
            }
            Ok(())
        }
    }

    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        use crate::ffi::routing;
        use fastrapi_http::exceptions::{PyWebSocketDisconnect, PyWebSocketException};
        use fastrapi_http::responses::PyResponse;
        use fastrapi_http::templating::PyJinja2Templates;
        use fastrapi_http::websocket::PyWebSocket;

        let py = m.py();
        m.setattr("__package__", "fastrapi")?;
        m.setattr("__path__", PyList::empty(py))?;
        m.add("FastAPI", py.get_type::<crate::engine::config::FastrAPI>())?;
        m.add("FastrAPI", py.get_type::<crate::engine::config::FastrAPI>())?;
        m.add("Response", py.get_type::<PyResponse>())?;
        m.add(
            "Middleware",
            py.import("starlette.middleware")?.getattr("Middleware")?,
        )?;
        m.add(
            "WSGIMiddleware",
            py.import("starlette.middleware.wsgi")?
                .getattr("WSGIMiddleware")?,
        )?;
        m.add(
            "BaseHTTPMiddleware",
            py.import("starlette.middleware.base")?
                .getattr("BaseHTTPMiddleware")?,
        )?;
        m.add(
            "TestClient",
            py.import("starlette.testclient")?.getattr("TestClient")?,
        )?;

        m.add("Jinja2Templates", py.get_type::<PyJinja2Templates>())?;
        let templating = PyModule::new(py, "templating")?;
        templating.add_class::<PyJinja2Templates>()?;
        m.add_submodule(&templating)?;
        let testclient = PyModule::new(py, "testclient")?;
        testclient.add(
            "TestClient",
            py.import("starlette.testclient")?.getattr("TestClient")?,
        )?;
        m.add_submodule(&testclient)?;

        let applications = PyModule::new(py, "applications")?;
        applications.add("FastAPI", py.get_type::<crate::engine::config::FastrAPI>())?;
        m.add_submodule(&applications)?;
        let websockets = PyModule::new(py, "websockets")?;
        websockets.add("WebSocket", py.get_type::<PyWebSocket>())?;
        websockets.add(
            "WebSocketDisconnect",
            py.get_type::<PyWebSocketDisconnect>(),
        )?;
        websockets.add("WebSocketException", py.get_type::<PyWebSocketException>())?;
        if let Ok(value) = py
            .import("starlette.websockets")
            .and_then(|module| module.getattr("WebSocketState"))
        {
            websockets.add("WebSocketState", value)?;
        }
        m.add_submodule(&websockets)?;
        let logger = PyModule::new(py, "logger")?;
        m.add_submodule(&logger)?;
        let types = PyModule::new(py, "types")?;
        m.add_submodule(&types)?;
        let middleware_exit = PyModule::new(py, "asyncexitstack")?;
        middleware_exit.add(
            "AsyncExitStackMiddleware",
            py.import("builtins")?.getattr("object")?,
        )?;
        py.import("sys")?
            .getattr("modules")?
            .set_item("fastrapi.middleware.asyncexitstack", &middleware_exit)?;
        m.getattr("middleware")?
            .setattr("__path__", PyList::empty(py))?;
        m.getattr("middleware")?
            .setattr("asyncexitstack", &middleware_exit)?;

        create_status_submodule(m)?;
        openapi::register(m)?;
        register_pydantic_integration(m)?;
        super::register_asyncio_alias(m)?;
        routing::register(m)?;
        // fastrapi.jsonable_encoder + fastrapi.encoders.jsonable_encoder
        encoders::register(m)?;
        register_compat_modules(m)?;
        m.add_function(wrap_pyfunction!(kernel_validation_count, m)?)?;
        let encoders = PyModule::new(py, "encoders")?;
        encoders::register(&encoders)?;
        m.add("encoders", encoders)?;
        super::register_submodules_in_sys_modules(m)?;

        Ok(())
    }
}
