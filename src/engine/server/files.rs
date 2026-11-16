//! The static file mounts and the frontend mounts of the server.
//! This module serves files from a directory and gives the fallback file for frontend navigation requests.

use crate::engine::config::{FrontendMount, StaticMount};
use crate::engine::exception_dispatch::{dispatch_http_error, resolve_pending_handler};
use crate::engine::server::routes::request_scheme;
use axum::Router;
use axum::body::Body;
use axum::extract::OriginalUri;
use axum::extract::Request;
use axum::http::header::{ACCEPT, CONTENT_TYPE, HOST, LOCATION};
use axum::http::{Extensions, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::response_api::content_type;
use fastrapi_runtime::py_bridge::EventLoop;
use smallvec::SmallVec;
use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::task::spawn_blocking;
use tower::{ServiceExt, service_fn};
use tower_http::services::{ServeDir, ServeFile};

pub(crate) async fn serve_frontend_mounts(
    mounts: Arc<Vec<FrontendMount>>,
    req: Request,
) -> Option<Response> {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return None;
    }

    let (mount, relative_path) = frontend_match(&mounts, req.uri().path())?;
    let file_path = frontend_safe_path(&mount.directory, relative_path)?;
    if fs::metadata(&file_path)
        .await
        .is_ok_and(|metadata| metadata.is_file())
    {
        return serve_frontend_file(req, file_path, StatusCode::OK).await;
    }

    let fallback = mount.fallback.as_deref()?;
    let navigation = frontend_navigation_request(&req, relative_path);
    let (fallback_path, status) = frontend_fallback_path(mount, fallback, navigation).await?;
    serve_frontend_file(req, fallback_path, status).await
}

async fn serve_frontend_file(req: Request, path: PathBuf, status: StatusCode) -> Option<Response> {
    let mut response = ServeFile::new(path)
        .oneshot(req)
        .await
        .ok()
        .map(IntoResponse::into_response)?;
    *response.status_mut() = status;
    Some(response)
}

struct HandledRequest {
    method: Method,
    headers: HeaderMap,
    server: EventLoop,
}

struct StaticOrigin {
    uri: Uri,
    scheme: &'static str,
    host: Option<HeaderValue>,
    handled: Option<HandledRequest>,
}

impl StaticOrigin {
    fn of(req: &Request, handlers: Option<EventLoop>) -> Self {
        Self {
            uri: req
                .extensions()
                .get::<OriginalUri>()
                .map_or_else(|| req.uri().clone(), |original| original.0.clone()),
            scheme: if request_scheme(req) == "https" {
                "https"
            } else {
                "http"
            },
            host: req.headers().get(HOST).cloned(),
            handled: handlers.map(|server| HandledRequest {
                method: req.method().clone(),
                headers: req.headers().clone(),
                server,
            }),
        }
    }

    async fn finish(&self, response: Response) -> Response {
        let status = response.status();
        if matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) && let Some(handled) = &self.handled
        {
            let extensions = Extensions::new();
            let input = RequestInput::new(
                &handled.method,
                &self.uri,
                &handled.headers,
                &extensions,
                Some(handled.server),
            );
            if let Some(response) = dispatch_http_error(handled.server, status.as_u16(), &input) {
                return resolve_pending_handler(response).await;
            }
        }
        self.finish_plain(response)
    }

    fn finish_plain(&self, mut response: Response) -> Response {
        let status = response.status();
        let error: &'static [u8] = match status {
            StatusCode::NOT_FOUND => br#"{"detail":"Not Found"}"#,
            StatusCode::METHOD_NOT_ALLOWED => br#"{"detail":"Method Not Allowed"}"#,
            _ if status.is_redirection() => {
                if let Ok(location) = HeaderValue::try_from(self.slash_url()) {
                    response.headers_mut().insert(LOCATION, location);
                }
                return response;
            }
            _ => {
                let charset = response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| {
                        let full = content_type(value, "utf-8");
                        (full.as_ref() != value)
                            .then(|| HeaderValue::try_from(full.as_ref()).ok())
                            .flatten()
                    });
                if let Some(charset) = charset {
                    response.headers_mut().insert(CONTENT_TYPE, charset);
                }
                return response;
            }
        };
        (
            status,
            [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
            error,
        )
            .into_response()
    }

    fn slash_url(&self) -> String {
        let query = self
            .uri
            .query()
            .map_or_else(String::new, |query| format!("?{query}"));
        match self.host.as_ref().and_then(|host| host.to_str().ok()) {
            Some(host) => format!("{}://{host}{}/{query}", self.scheme, self.uri.path()),
            None => format!("{}/{query}", self.uri.path()),
        }
    }
}

pub(crate) fn add_static_mount(
    app: Router,
    mount: StaticMount,
    handlers: Option<EventLoop>,
) -> Router {
    let mount_path = if mount.path == "/" {
        "/".to_string()
    } else {
        mount.path.trim_end_matches('/').to_string()
    };

    if mount.directories.len() > 1 {
        let services = Arc::new(
            mount
                .directories
                .iter()
                .map(|directory| {
                    ServeDir::new(directory).append_index_html_on_directories(mount.html)
                })
                .collect::<Vec<_>>(),
        );
        let service = service_fn(move |req: Request| {
            let services = services.clone();
            let origin = StaticOrigin::of(&req, handlers);
            async move {
                let (parts, _) = req.into_parts();
                for service in services.iter() {
                    let candidate = Request::from_parts(parts.clone(), Body::empty());
                    let response = service
                        .clone()
                        .oneshot(candidate)
                        .await
                        .map(IntoResponse::into_response)?;
                    if response.status() != StatusCode::NOT_FOUND {
                        return Ok::<_, Infallible>(origin.finish(response).await);
                    }
                }
                Ok(origin.finish(StatusCode::NOT_FOUND.into_response()).await)
            }
        });
        return app.nest_service(&mount_path, service);
    }

    let serve_dir = ServeDir::new(&mount.directory).append_index_html_on_directories(mount.html);
    if mount.follow_symlink {
        let service = service_fn(move |req: Request| {
            let serve_dir = serve_dir.clone();
            let origin = StaticOrigin::of(&req, handlers);
            async move {
                let response = serve_dir.oneshot(req).await?.into_response();
                Ok::<_, Infallible>(origin.finish(response).await)
            }
        });
        return app.nest_service(&mount_path, service);
    }

    let root = Arc::new(StaticRoot::new(PathBuf::from(mount.directory)));
    let index = if mount.html {
        DirectoryIndex::IndexHtml
    } else {
        DirectoryIndex::None
    };
    let service = service_fn(move |req: Request| {
        let root = root.clone();
        let serve_dir = serve_dir.clone();
        let origin = StaticOrigin::of(&req, handlers);
        async move {
            if static_request_escapes(root, req.uri().path(), index).await {
                let response = StatusCode::NOT_FOUND.into_response();
                return Ok::<_, Infallible>(origin.finish(response).await);
            }
            let response = serve_dir.oneshot(req).await?.into_response();
            Ok(origin.finish(response).await)
        }
    });

    app.nest_service(&mount_path, service)
}

fn frontend_safe_path(directory: &Path, request_path: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(request_path)
        .decode_utf8()
        .ok()?;
    let mut file_path = directory.to_path_buf();

    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => file_path.push(part),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }

    if request_path.is_empty() || request_path.ends_with('/') {
        file_path.push("index.html");
    }

    Some(file_path)
}

fn frontend_navigation_request(req: &Request, relative_path: &str) -> bool {
    relative_path
        .rsplit('/')
        .next()
        .is_none_or(|last| !last.contains('.'))
        && req
            .headers()
            .get(ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html") || accept.contains("*/*"))
}

async fn frontend_fallback_path(
    mount: &FrontendMount,
    fallback: &str,
    navigation: bool,
) -> Option<(PathBuf, StatusCode)> {
    let directory = mount.directory.as_path();
    let candidates: SmallVec<[(PathBuf, StatusCode); 2]> = if fallback == "auto" {
        smallvec::smallvec![
            (directory.join("404.html"), StatusCode::NOT_FOUND),
            (directory.join("index.html"), StatusCode::OK),
        ]
    } else if fallback == "404.html" {
        smallvec::smallvec![(directory.join(fallback), StatusCode::NOT_FOUND)]
    } else if navigation {
        smallvec::smallvec![(directory.join(fallback), StatusCode::OK)]
    } else {
        return None;
    };

    for (path, status) in candidates {
        if status == StatusCode::OK && !navigation {
            continue;
        }
        if fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Some((path, status));
        }
    }

    None
}

fn frontend_match<'a, 'p>(
    mounts: &'a [FrontendMount],
    request_path: &'p str,
) -> Option<(&'a FrontendMount, &'p str)> {
    mounts
        .iter()
        .filter_map(|mount| {
            if mount.path == "/" {
                return Some((mount, request_path.trim_start_matches('/')));
            }
            if request_path == mount.path {
                return Some((mount, ""));
            }
            request_path
                .strip_prefix(mount.path.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .map(|relative| (mount, relative))
        })
        .max_by_key(|(mount, _)| mount.path.len())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectoryIndex {
    None,
    IndexHtml,
}

/// The directory of a `StaticFiles(follow_symlink=False)` mount, resolved
/// once at mount time like Starlette's `os.path.realpath(directory)`.
struct StaticRoot {
    directory: PathBuf,
    resolved: Option<PathBuf>,
}

impl StaticRoot {
    fn new(directory: PathBuf) -> Self {
        let resolved = std::fs::canonicalize(&directory).ok();
        Self {
            directory,
            resolved,
        }
    }
}

/// Whether the request resolves, through a symlink, to a path outside the
/// mount directory. Like Starlette's `StaticFiles.lookup_path` without
/// `follow_symlink`, such a path is not found. The check runs in a single
/// blocking task.
async fn static_request_escapes(
    root: Arc<StaticRoot>,
    request_path: &str,
    index: DirectoryIndex,
) -> bool {
    let Ok(decoded) = percent_encoding::percent_decode_str(request_path).decode_utf8() else {
        return false;
    };
    let mut relative = PathBuf::new();
    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return false,
        }
    }
    spawn_blocking(move || {
        let Some(resolved_root) = root
            .resolved
            .clone()
            .or_else(|| std::fs::canonicalize(&root.directory).ok())
        else {
            return false;
        };
        let joined = root.directory.join(&relative);
        let escapes = |path: &Path| {
            std::fs::canonicalize(path).is_ok_and(|real| !real.starts_with(&resolved_root))
        };
        if escapes(&joined) {
            return true;
        }
        index == DirectoryIndex::IndexHtml
            && std::fs::metadata(&joined).is_ok_and(|metadata| metadata.is_dir())
            && escapes(&joined.join("index.html"))
    })
    .await
    .unwrap_or(false)
}
