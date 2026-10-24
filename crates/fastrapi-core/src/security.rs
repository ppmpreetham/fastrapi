use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecurityKind {
    OAuth2PasswordBearer {
        token_url: String,
        auto_error: bool,
    },
    OAuth2AuthorizationCode {
        authorization_url: String,
        token_url: String,
        refresh_url: Option<String>,
        auto_error: bool,
    },
    OpenIdConnect {
        url: String,
        auto_error: bool,
    },
    HTTPBearer {
        auto_error: bool,
        bearer_format: Option<String>,
    },
    HTTPBasic {
        auto_error: bool,
    },
    HTTPDigest {
        auto_error: bool,
    },
    APIKeyHeader {
        name: String,
        auto_error: bool,
    },
    APIKeyQuery {
        name: String,
        auto_error: bool,
    },
    APIKeyCookie {
        name: String,
        auto_error: bool,
    },
}

#[derive(Clone, Debug)]
pub struct CompiledSecurityScheme {
    pub id: u32,
    pub name: String,
    pub description: Option<String>,
    pub scopes: Option<simd_json::OwnedValue>,
    pub kind: SecurityKind,
}

#[derive(Clone, Debug)]
pub struct RouteSecurityRequirement {
    pub scheme: Arc<CompiledSecurityScheme>,
    pub scopes: Arc<[String]>,
}
