#![allow(clippy::result_large_err)]
#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule, PyString};

pub mod engine;
pub mod error;
pub mod ffi;
mod globals;
pub mod http;
pub mod macros;
pub mod routing;
pub mod runtime;
pub mod types;
pub mod utils;

pub use engine::{app, background, server};
pub use ffi::{datastructures, decorators, exceptions, pydantic};
pub use http::{middleware, request, responses, staticfiles, status, websocket};
pub use routing::{dependencies, params, security};
pub use runtime::executor;
pub use runtime::executor as py_handlers;

pub use app::FastrAPI;
pub use background::PyBackgroundTasks;
pub use datastructures::PyUploadFile;
pub use decorators::PyAPIRouter;
pub use engine::metrics::PyInstrumentator;
pub use exceptions::{
    PyDependencyScopeError, PyFastAPIDeprecationWarning, PyFastAPIError, PyHTTPException,
    PyPydanticV1NotSupportedError, PyRequestValidationError, PyResponseValidationError,
    PyValidationException, PyWebSocketException, PyWebSocketRequestValidationError,
};
pub use middleware::{
    CORSMiddleware, GZipMiddleware, HTTPSRedirectMiddleware, SessionMiddleware,
    TrustedHostMiddleware,
};
pub use params::{
    PyBody, PyCookie, PyDepends, PyFile, PyForm, PyHeader, PyPath, PyQuery, PySecurity, Undefined,
    Unset,
};
pub use request::{PyHTTPConnection, PyRequest};
pub use responses::{
    PyFileResponse, PyHTMLResponse, PyJSONResponse, PyORJSONResponse, PyPlainTextResponse,
    PyRedirectResponse, PyStreamingResponse, PyUJSONResponse,
};
pub use routing::security::{
    APIKeyCookie, APIKeyHeader, APIKeyQuery, HTTPAuthorizationCredentials, HTTPBasic,
    HTTPBasicCredentials, HTTPBearer, OAuth2AuthorizationCodeBearer, OAuth2PasswordBearer,
    OpenIdConnect, PySecurityScopes,
};
pub use staticfiles::PyStaticFiles;
pub use websocket::PyWebSocket;

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
        visited: &mut std::collections::HashSet<usize>,
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

    walk(m, "fastrapi", &mut Default::default())?;
    Ok(())
}

#[pymodule(gil_used = false)]
mod fastrapi {
    use pyo3::prelude::*;
    use pyo3::types::{PyList, PyModule};

    // Top-level exported classes
    #[pymodule_export]
    use crate::FastrAPI;

    #[pymodule_export]
    use crate::PyAPIRouter as APIRouter;

    #[pymodule_export]
    use crate::PyHTTPConnection as HTTPConnection;

    #[pymodule_export]
    use crate::PyRequest as Request;

    #[pymodule_export]
    use crate::PyBackgroundTasks as BackgroundTasks;

    #[pymodule_export]
    use crate::PyUploadFile as UploadFile;

    #[pymodule_export]
    use crate::PyStaticFiles as StaticFiles;

    #[pymodule_export]
    use crate::PyHTTPException as HTTPException;

    #[pymodule_export]
    use crate::PyBody as Body;
    #[pymodule_export]
    use crate::PyCookie as Cookie;
    #[pymodule_export]
    use crate::PyDepends as Depends;
    #[pymodule_export]
    use crate::PyFile as File;
    #[pymodule_export]
    use crate::PyForm as Form;
    #[pymodule_export]
    use crate::PyHeader as Header;
    #[pymodule_export]
    use crate::PyPath as Path;
    #[pymodule_export]
    use crate::PyQuery as Query;
    #[pymodule_export]
    use crate::PySecurity as Security;

    #[pymodule_export]
    use crate::APIKeyCookie;
    #[pymodule_export]
    use crate::APIKeyHeader;
    #[pymodule_export]
    use crate::APIKeyQuery;
    #[pymodule_export]
    use crate::HTTPAuthorizationCredentials;
    #[pymodule_export]
    use crate::HTTPBasic;
    #[pymodule_export]
    use crate::HTTPBasicCredentials;
    #[pymodule_export]
    use crate::HTTPBearer;
    #[pymodule_export]
    use crate::OAuth2PasswordBearer;
    #[pymodule_export]
    use crate::PySecurityScopes as SecurityScopes;

    #[pymodule_export]
    use crate::PyInstrumentator as Instrumentator;

    #[pymodule]
    mod responses {
        #[pymodule_export]
        use crate::responses::{
            PyFileResponse as FileResponse, PyHTMLResponse as HTMLResponse,
            PyJSONResponse as JSONResponse, PyORJSONResponse as ORJSONResponse,
            PyPlainTextResponse as PlainTextResponse, PyRedirectResponse as RedirectResponse,
            PyStreamingResponse as StreamingResponse, PyUJSONResponse as UJSONResponse,
        };
    }

    #[pymodule]
    mod exceptions {
        #[pymodule_export]
        use crate::exceptions::{
            PyDependencyScopeError as DependencyScopeError,
            PyFastAPIDeprecationWarning as FastAPIDeprecationWarning,
            PyFastAPIError as FastAPIError, PyHTTPException as HTTPException,
            PyPydanticV1NotSupportedError as PydanticV1NotSupportedError,
            PyRequestValidationError as RequestValidationError,
            PyResponseValidationError as ResponseValidationError,
            PyValidationException as ValidationException,
            PyWebSocketException as WebSocketException,
            PyWebSocketRequestValidationError as WebSocketRequestValidationError,
        };
    }

    #[pymodule]
    mod params {
        #[pymodule_export]
        use crate::params::{
            PyBody as Body, PyCookie as Cookie, PyDepends as Depends, PyFile as File,
            PyForm as Form, PyHeader as Header, PyPath as Path, PyQuery as Query,
            PySecurity as Security, Undefined, Unset,
        };
    }

    #[pymodule]
    mod request {
        #[pymodule_export]
        use crate::request::{
            PyAppInfo as AppInfo, PyClientInfo as ClientInfo, PyHTTPConnection as HTTPConnection,
            PyRequest as Request,
        };
    }

    #[pymodule]
    mod requests {
        #[pymodule_export]
        use crate::request::{
            PyAppInfo as AppInfo, PyClientInfo as ClientInfo, PyHTTPConnection as HTTPConnection,
            PyRequest as Request,
        };
    }

    #[pymodule]
    mod datastructures {
        #[pymodule_export]
        use crate::datastructures::PyUploadFile as UploadFile;
    }

    #[pymodule]
    mod background {
        #[pymodule_export]
        use crate::background::PyBackgroundTasks as BackgroundTasks;
    }

    #[pymodule]
    mod security {
        #[pymodule_export]
        use crate::security::{
            APIKeyCookie, APIKeyHeader, APIKeyQuery, HTTPAuthorizationCredentials, HTTPBasic,
            HTTPBasicCredentials, HTTPBearer, HTTPDigest, OAuth2AuthorizationCodeBearer,
            OAuth2PasswordBearer, OpenIdConnect, PySecurityScopes as SecurityScopes,
        };
    }

    #[pymodule]
    mod staticfiles {
        #[pymodule_export]
        use crate::staticfiles::PyStaticFiles as StaticFiles;
    }

    #[pymodule]
    mod middleware {
        use pyo3::prelude::*;

        #[pymodule_export]
        use crate::middleware::{
            CORSMiddleware, GZipMiddleware, HTTPSRedirectMiddleware, SessionMiddleware,
            TrustedHostMiddleware,
        };

        #[pymodule]
        mod cors {

            #[pymodule_export]
            use crate::middleware::CORSMiddleware;
        }
    }

    #[pymodule]
    mod prometheus {
        #[pymodule_export]
        use crate::engine::metrics::PyInstrumentator as Instrumentator;
    }

    #[pymodule]
    mod websocket {
        #[pymodule_export]
        use crate::websocket::PyWebSocket as WebSocket;
    }

    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        let py = m.py();
        m.setattr("__package__", "fastrapi")?;
        m.setattr("__path__", PyList::empty(py))?;

        crate::status::create_status_submodule(m)?;
        crate::pydantic::register_pydantic_integration(m)?;
        super::register_asyncio_alias(m)?;
        // fastrapi.jsonable_encoder + fastrapi.encoders.jsonable_encoder
        crate::ffi::encoders::register(m)?;
        m.add_function(wrap_pyfunction!(
            crate::ffi::kernel_validation::kernel_validation_count,
            m
        )?)?;
        let encoders = PyModule::new(py, "encoders")?;
        crate::ffi::encoders::register(&encoders)?;
        m.add("encoders", encoders)?;
        super::register_submodules_in_sys_modules(m)?;

        Ok(())
    }
}
