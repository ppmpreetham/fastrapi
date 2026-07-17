use crate::engine::types::{FrontendMount, StaticMount};
use axum::{
    Router,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use smallvec::SmallVec;
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tower::{ServiceExt, service_fn};
use tower_http::services::{ServeDir, ServeFile};

pub(crate) async fn serve_frontend_mounts(
    mounts: Arc<Vec<FrontendMount>>,
    req: Request,
) -> Option<Response> {
    if !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) {
        return None;
    }

    let (mount, relative_path) = frontend_match(&mounts, req.uri().path())?;
    let file_path = frontend_safe_path(&mount.directory, &relative_path)?;
    if tokio::fs::metadata(&file_path)
        .await
        .is_ok_and(|metadata| metadata.is_file())
    {
        return serve_frontend_file(req, file_path, StatusCode::OK).await;
    }

    let fallback = mount.fallback.as_deref()?;
    let navigation = frontend_navigation_request(&req, &relative_path);
    let (fallback_path, status) = frontend_fallback_path(mount, fallback, navigation).await?;
    serve_frontend_file(req, fallback_path, status).await
}

pub(crate) async fn serve_frontend_file(
    req: Request,
    path: PathBuf,
    status: StatusCode,
) -> Option<Response> {
    let mut response = ServeFile::new(path)
        .oneshot(req)
        .await
        .ok()
        .map(IntoResponse::into_response)?;
    *response.status_mut() = status;
    Some(response)
}

pub(crate) fn add_static_mount(app: Router, mount: StaticMount) -> Router {
    let mount_path = if mount.path == "/" {
        "/".to_string()
    } else {
        mount.path.trim_end_matches('/').to_string()
    };

    let serve_dir = ServeDir::new(&mount.directory).append_index_html_on_directories(mount.html);
    if mount.follow_symlink {
        return app.nest_service(&mount_path, serve_dir);
    }

    let directory = Arc::new(PathBuf::from(mount.directory));
    let html = mount.html;
    let service = service_fn(move |req: Request| {
        let directory = directory.clone();
        let serve_dir = serve_dir.clone();
        async move {
            if static_request_hits_symlink(&directory, req.uri().path(), html).await {
                return Ok::<_, std::convert::Infallible>(StatusCode::FORBIDDEN.into_response());
            }

            serve_dir
                .oneshot(req)
                .await
                .map(IntoResponse::into_response)
        }
    });

    app.nest_service(&mount_path, service)
}

pub(crate) fn frontend_safe_path(directory: &str, request_path: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(request_path)
        .decode_utf8()
        .ok()?;
    let mut file_path = PathBuf::from(directory);

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

pub(crate) fn frontend_navigation_request(req: &Request, relative_path: &str) -> bool {
    relative_path
        .rsplit('/')
        .next()
        .is_none_or(|last| !last.contains('.'))
        && req
            .headers()
            .get(axum::http::header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html") || accept.contains("*/*"))
}

pub(crate) async fn frontend_fallback_path(
    mount: &FrontendMount,
    fallback: &str,
    navigation: bool,
) -> Option<(PathBuf, StatusCode)> {
    let directory = Path::new(&mount.directory);
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
        if tokio::fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Some((path, status));
        }
    }

    None
}

pub(crate) fn frontend_match<'a>(
    mounts: &'a [FrontendMount],
    request_path: &str,
) -> Option<(&'a FrontendMount, String)> {
    mounts
        .iter()
        .filter_map(|mount| {
            if mount.path == "/" {
                return Some((mount, request_path.trim_start_matches('/').to_string()));
            }
            if request_path == mount.path {
                return Some((mount, String::new()));
            }
            request_path
                .strip_prefix(&format!("{}/", mount.path))
                .map(|relative| (mount, relative.to_string()))
        })
        .max_by_key(|(mount, _)| mount.path.len())
}

pub(crate) async fn static_request_hits_symlink(
    directory: &Path,
    request_path: &str,
    html: bool,
) -> bool {
    let decoded = match percent_encoding::percent_decode_str(request_path).decode_utf8() {
        Ok(decoded) => decoded,
        Err(_) => return false,
    };
    if tokio::fs::symlink_metadata(directory)
        .await
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return true;
    }

    let mut file_path = directory.to_path_buf();

    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => file_path.push(part),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return false,
        }

        if tokio::fs::symlink_metadata(&file_path)
            .await
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return true;
        }
    }

    if html
        && let Ok(metadata) = tokio::fs::metadata(&file_path).await
        && metadata.is_dir()
    {
        file_path.push("index.html");
        return tokio::fs::symlink_metadata(&file_path)
            .await
            .is_ok_and(|m| m.file_type().is_symlink());
    }

    false
}
