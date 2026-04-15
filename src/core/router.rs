use crate::core::handler::RouteHandler;
use once_cell::sync::Lazy;
use papaya::HashMap as PapayaHashMap;

pub static ROUTES: Lazy<PapayaHashMap<String, RouteHandler>> =
    Lazy::new(|| PapayaHashMap::with_capacity(128));
