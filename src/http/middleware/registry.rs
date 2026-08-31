use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

use super::PyMiddleware;
use super::cors::{CORSMiddleware, parse_cors_params};
use super::gzip::{GZipMiddleware, parse_gzip_params};
use super::httpsredirect::{HTTPSRedirectMiddleware, parse_https_redirect_params};
use super::session::{SessionMiddleware, parse_session_params};
use super::trustedhost::{TrustedHostMiddleware, parse_trusted_host_params};

#[derive(Clone)]
pub enum DeclaredLayer {
    Cors,
    TrustedHost,
    HttpsRedirect,
    GZip,
    Session,
    Custom(Arc<PyMiddleware>),
}

#[derive(Clone, Default)]
pub struct MiddlewareContainer {
    pub cors: Option<CORSMiddleware>,
    pub trusted_host: Option<TrustedHostMiddleware>,
    pub https_redirect: Option<HTTPSRedirectMiddleware>,
    pub gzip: Option<GZipMiddleware>,
    pub session: Option<SessionMiddleware>,
    pub py_middlewares: Vec<Arc<PyMiddleware>>,
    pub order: Vec<DeclaredLayer>,
}

impl MiddlewareContainer {
    pub(crate) fn record_layer(&mut self, class_name: &str) {
        if let Some(builder) = MIDDLEWARE_REGISTRY.get(class_name) {
            self.order.push(builder.layer());
        }
    }
}

pub trait MiddlewareBuilder: Send + Sync {
    fn name(&self) -> &'static str;
    fn layer(&self) -> DeclaredLayer;

    fn try_from_instance(
        &self,
        item: &Bound<'_, PyAny>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<bool>;

    fn parse_kwargs(
        &self,
        kwargs: &Bound<'_, PyDict>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<()>;
}

struct CorsMiddlewareBuilder;

impl MiddlewareBuilder for CorsMiddlewareBuilder {
    fn name(&self) -> &'static str {
        "CORSMiddleware"
    }

    fn layer(&self) -> DeclaredLayer {
        DeclaredLayer::Cors
    }

    fn try_from_instance(
        &self,
        item: &Bound<'_, PyAny>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<bool> {
        if let Ok(config_bound) = item.cast::<CORSMiddleware>() {
            container.cors = Some(config_bound.borrow().clone());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_kwargs(
        &self,
        kwargs: &Bound<'_, PyDict>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<()> {
        container.cors = Some(parse_cors_params(kwargs)?);
        Ok(())
    }
}

struct TrustedHostMiddlewareBuilder;

impl MiddlewareBuilder for TrustedHostMiddlewareBuilder {
    fn name(&self) -> &'static str {
        "TrustedHostMiddleware"
    }

    fn layer(&self) -> DeclaredLayer {
        DeclaredLayer::TrustedHost
    }

    fn try_from_instance(
        &self,
        item: &Bound<'_, PyAny>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<bool> {
        if let Ok(config_bound) = item.cast::<TrustedHostMiddleware>() {
            container.trusted_host = Some(config_bound.borrow().clone());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_kwargs(
        &self,
        kwargs: &Bound<'_, PyDict>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<()> {
        container.trusted_host = Some(parse_trusted_host_params(kwargs)?);
        Ok(())
    }
}

struct HttpsRedirectMiddlewareBuilder;

impl MiddlewareBuilder for HttpsRedirectMiddlewareBuilder {
    fn name(&self) -> &'static str {
        "HTTPSRedirectMiddleware"
    }

    fn layer(&self) -> DeclaredLayer {
        DeclaredLayer::HttpsRedirect
    }

    fn try_from_instance(
        &self,
        item: &Bound<'_, PyAny>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<bool> {
        if let Ok(config_bound) = item.cast::<HTTPSRedirectMiddleware>() {
            container.https_redirect = Some(config_bound.borrow().clone());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_kwargs(
        &self,
        kwargs: &Bound<'_, PyDict>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<()> {
        container.https_redirect = Some(parse_https_redirect_params(kwargs)?);
        Ok(())
    }
}

struct GZipMiddlewareBuilder;

impl MiddlewareBuilder for GZipMiddlewareBuilder {
    fn name(&self) -> &'static str {
        "GZipMiddleware"
    }

    fn layer(&self) -> DeclaredLayer {
        DeclaredLayer::GZip
    }

    fn try_from_instance(
        &self,
        item: &Bound<'_, PyAny>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<bool> {
        if let Ok(config_bound) = item.cast::<GZipMiddleware>() {
            container.gzip = Some(config_bound.borrow().clone());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_kwargs(
        &self,
        kwargs: &Bound<'_, PyDict>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<()> {
        container.gzip = Some(parse_gzip_params(kwargs)?);
        Ok(())
    }
}

struct SessionMiddlewareBuilder;

impl MiddlewareBuilder for SessionMiddlewareBuilder {
    fn name(&self) -> &'static str {
        "SessionMiddleware"
    }

    fn layer(&self) -> DeclaredLayer {
        DeclaredLayer::Session
    }

    fn try_from_instance(
        &self,
        item: &Bound<'_, PyAny>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<bool> {
        if let Ok(config_bound) = item.cast::<SessionMiddleware>() {
            container.session = Some(config_bound.borrow().clone());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_kwargs(
        &self,
        kwargs: &Bound<'_, PyDict>,
        container: &mut MiddlewareContainer,
    ) -> PyResult<()> {
        container.session = Some(parse_session_params(kwargs)?);
        Ok(())
    }
}

pub struct MiddlewareRegistry {
    builders: Vec<Box<dyn MiddlewareBuilder>>,
    name_map: HashMap<&'static str, usize>,
}

impl MiddlewareRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            builders: Vec::new(),
            name_map: HashMap::new(),
        };
        registry.register(Box::new(CorsMiddlewareBuilder));
        registry.register(Box::new(TrustedHostMiddlewareBuilder));
        registry.register(Box::new(HttpsRedirectMiddlewareBuilder));
        registry.register(Box::new(GZipMiddlewareBuilder));
        registry.register(Box::new(SessionMiddlewareBuilder));
        registry
    }

    pub fn register(&mut self, builder: Box<dyn MiddlewareBuilder>) {
        let name = builder.name();
        let idx = self.builders.len();
        self.builders.push(builder);
        self.name_map.insert(name, idx);
    }

    pub fn get(&self, name: &str) -> Option<&dyn MiddlewareBuilder> {
        self.name_map
            .get(name)
            .map(|&idx| self.builders[idx].as_ref())
    }

    pub fn builders(&self) -> &[Box<dyn MiddlewareBuilder>] {
        &self.builders
    }

    pub fn supported_names(&self) -> Vec<&'static str> {
        self.builders.iter().map(|b| b.name()).collect()
    }
}

impl Default for MiddlewareRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub static MIDDLEWARE_REGISTRY: LazyLock<MiddlewareRegistry> =
    LazyLock::new(MiddlewareRegistry::new);
