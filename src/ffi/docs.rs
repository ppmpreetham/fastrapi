//! The HTML pages of the interactive API docs: Swagger UI, ReDoc and the Swagger UI OAuth2 redirect.
//! The Python functions copy `fastapi.openapi.docs`, and the server uses the same builders for its docs routes.

use fastrapi_http::responses::PyHTMLResponse;
use fastrapi_pyutil::json::write_py_json;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use std::fmt::Write;

pub(crate) const SWAGGER_JS_URL: &str =
    "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js";
pub(crate) const SWAGGER_CSS_URL: &str =
    "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui.css";
pub(crate) const FAVICON_URL: &str = "https://fastapi.tiangolo.com/img/favicon.png";
pub(crate) const REDOC_JS_URL: &str =
    "https://cdn.jsdelivr.net/npm/redoc@2/bundles/redoc.standalone.js";

/// The HTML page that Swagger UI opens after an OAuth2 login.
pub(crate) const OAUTH2_REDIRECT_HTML: &str = r#"
    <!doctype html>
    <html lang="en-US">
    <head>
        <title>Swagger UI: OAuth2 Redirect</title>
    </head>
    <body>
    <script>
        'use strict';
        function run () {
            var oauth2 = window.opener.swaggerUIRedirectOauth2;
            var sentState = oauth2.state;
            var redirectUrl = oauth2.redirectUrl;
            var isValid, qp, arr;

            if (/code|token|error/.test(window.location.hash)) {
                qp = window.location.hash.substring(1).replace('?', '&');
            } else {
                qp = location.search.substring(1);
            }

            arr = qp.split("&");
            arr.forEach(function (v,i,_arr) { _arr[i] = '"' + v.replace('=', '":"') + '"';});
            qp = qp ? JSON.parse('{' + arr.join() + '}',
                    function (key, value) {
                        return key === "" ? value : decodeURIComponent(value);
                    }
            ) : {};

            isValid = qp.state === sentState;

            if ((
              oauth2.auth.schema.get("flow") === "accessCode" ||
              oauth2.auth.schema.get("flow") === "authorizationCode" ||
              oauth2.auth.schema.get("flow") === "authorization_code"
            ) && !oauth2.auth.code) {
                if (!isValid) {
                    oauth2.errCb({
                        authId: oauth2.auth.name,
                        source: "auth",
                        level: "warning",
                        message: "Authorization may be unsafe, passed state was changed in server. The passed state wasn't returned from auth server."
                    });
                }

                if (qp.code) {
                    delete oauth2.state;
                    oauth2.auth.code = qp.code;
                    oauth2.callback({auth: oauth2.auth, redirectUrl: redirectUrl});
                } else {
                    let oauthErrorMsg;
                    if (qp.error) {
                        oauthErrorMsg = "["+qp.error+"]: " +
                            (qp.error_description ? qp.error_description+ ". " : "no accessCode received from the server. ") +
                            (qp.error_uri ? "More info: "+qp.error_uri : "");
                    }

                    oauth2.errCb({
                        authId: oauth2.auth.name,
                        source: "auth",
                        level: "error",
                        message: oauthErrorMsg || "[Authorization failed]: no accessCode received from the server."
                    });
                }
            } else {
                oauth2.callback({auth: oauth2.auth, token: qp, isValid: isValid, redirectUrl: redirectUrl});
            }
            window.close();
        }

        if (document.readyState !== 'loading') {
            run();
        } else {
            document.addEventListener('DOMContentLoaded', function () {
                run();
            });
        }
    </script>
    </body>
    </html>
        "#;

/// The settings that FastAPI gives to Swagger UI when the app does not change them.
pub(crate) fn swagger_ui_default_parameters(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let params = PyDict::new(py);
    params.set_item("dom_id", "#swagger-ui")?;
    params.set_item("layout", "BaseLayout")?;
    params.set_item("deepLinking", true)?;
    params.set_item("showExtensions", true)?;
    params.set_item("showCommonExtensions", true)?;
    Ok(params)
}

fn html_safe_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
    let mut buffer = Vec::new();
    write_py_json(py, value, &mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer)
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026"))
}

/// The Swagger UI page settings, in the order of `get_swagger_ui_html`.
pub(crate) struct SwaggerPage<'a, 'py> {
    pub openapi_url: &'a str,
    pub title: &'a str,
    pub swagger_js_url: &'a str,
    pub swagger_css_url: &'a str,
    pub swagger_favicon_url: &'a str,
    pub oauth2_redirect_url: Option<&'a str>,
    pub init_oauth: Option<&'a Bound<'py, PyAny>>,
    pub swagger_ui_parameters: Option<&'a Bound<'py, PyAny>>,
}

/// Builds the Swagger UI HTML page.
pub(crate) fn swagger_ui_page(py: Python<'_>, page: &SwaggerPage<'_, '_>) -> PyResult<String> {
    let parameters = swagger_ui_default_parameters(py)?;
    if let Some(custom) = page.swagger_ui_parameters
        && custom.is_truthy()?
    {
        parameters.call_method1("update", (custom,))?;
    }
    let mut html = format!(
        r#"
    <!DOCTYPE html>
    <html>
    <head>
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <link type="text/css" rel="stylesheet" href="{css}">
    <link rel="shortcut icon" href="{favicon}">
    <title>{title}</title>
    </head>
    <body>
    <div id="swagger-ui">
    </div>
    <script src="{js}"></script>
    <!-- `SwaggerUIBundle` is now available on the page -->
    <script>
    const ui = SwaggerUIBundle({{
        url: '{url}',
    "#,
        css = page.swagger_css_url,
        favicon = page.swagger_favicon_url,
        title = page.title,
        js = page.swagger_js_url,
        url = page.openapi_url,
    );
    for (key, value) in parameters.iter() {
        _ = writeln!(
            html,
            "{}: {},",
            html_safe_json(py, &key)?,
            html_safe_json(py, &value)?
        );
    }
    if let Some(redirect) = page.oauth2_redirect_url {
        _ = write!(
            html,
            "oauth2RedirectUrl: window.location.origin + '{redirect}',"
        );
    }
    html.push_str(
        "
    presets: [
        SwaggerUIBundle.presets.apis,
        SwaggerUIBundle.SwaggerUIStandalonePreset
        ],
    })",
    );
    if let Some(init_oauth) = page.init_oauth
        && init_oauth.is_truthy()?
    {
        _ = write!(
            html,
            "
        ui.initOAuth({})
        ",
            html_safe_json(py, init_oauth)?
        );
    }
    html.push_str(
        "
    </script>
    </body>
    </html>
    ",
    );
    Ok(html)
}

/// Builds the ReDoc HTML page.
pub(crate) fn redoc_page(
    openapi_url: &str,
    title: &str,
    redoc_js_url: &str,
    redoc_favicon_url: &str,
    with_google_fonts: bool,
) -> String {
    let fonts = if with_google_fonts {
        r#"
    <link href="https://fonts.googleapis.com/css?family=Montserrat:300,400,700|Roboto:300,400,700" rel="stylesheet">
    "#
    } else {
        ""
    };
    format!(
        r#"
    <!DOCTYPE html>
    <html>
    <head>
    <title>{title}</title>
    <!-- needed for adaptive design -->
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1">
    {fonts}
    <link rel="shortcut icon" href="{redoc_favicon_url}">
    <!--
    ReDoc doesn't change outer page styles
    -->
    <style>
      body {{
        margin: 0;
        padding: 0;
      }}
    </style>
    </head>
    <body>
    <noscript>
        ReDoc requires Javascript to function. Please enable it to browse the documentation.
    </noscript>
    <redoc spec-url="{openapi_url}"></redoc>
    <script src="{redoc_js_url}"> </script>
    </body>
    </html>
    "#
    )
}

fn html_response(py: Python<'_>, html: String) -> PyResult<Py<PyAny>> {
    Ok(py.get_type::<PyHTMLResponse>().call1((html,))?.unbind())
}

/// Generate and return the HTML that loads Swagger UI for the interactive
/// API docs (normally served at `/docs`).
///
/// You would only call this function yourself if you needed to override some parts,
/// for example the URLs to use to load Swagger UI's JavaScript and CSS.
///
/// Read more about it in the
/// [FastAPI docs for Configure Swagger UI](https://fastapi.tiangolo.com/how-to/configure-swagger-ui/)
/// and the [FastAPI docs for Custom Docs UI Static Assets (Self-Hosting)](https://fastapi.tiangolo.com/how-to/custom-docs-ui-assets/).
#[pyfunction]
#[pyo3(signature = (
    *,
    openapi_url,
    title,
    swagger_js_url = SWAGGER_JS_URL,
    swagger_css_url = SWAGGER_CSS_URL,
    swagger_favicon_url = FAVICON_URL,
    oauth2_redirect_url = None,
    init_oauth = None,
    swagger_ui_parameters = None
))]
#[expect(
    clippy::too_many_arguments,
    reason = "the signature copies FastAPI's get_swagger_ui_html"
)]
fn get_swagger_ui_html(
    py: Python<'_>,
    openapi_url: &str,
    title: &str,
    swagger_js_url: &str,
    swagger_css_url: &str,
    swagger_favicon_url: &str,
    oauth2_redirect_url: Option<&str>,
    init_oauth: Option<&Bound<'_, PyAny>>,
    swagger_ui_parameters: Option<&Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let html = swagger_ui_page(
        py,
        &SwaggerPage {
            openapi_url,
            title,
            swagger_js_url,
            swagger_css_url,
            swagger_favicon_url,
            oauth2_redirect_url,
            init_oauth,
            swagger_ui_parameters,
        },
    )?;
    html_response(py, html)
}

/// Generate and return the HTML response that loads ReDoc for the alternative
/// API docs (normally served at `/redoc`).
///
/// You would only call this function yourself if you needed to override some parts,
/// for example the URLs to use to load ReDoc's JavaScript and CSS.
///
/// Read more about it in the
/// [FastAPI docs for Custom Docs UI Static Assets (Self-Hosting)](https://fastapi.tiangolo.com/how-to/custom-docs-ui-assets/).
#[pyfunction]
#[pyo3(signature = (
    *,
    openapi_url,
    title,
    redoc_js_url = REDOC_JS_URL,
    redoc_favicon_url = FAVICON_URL,
    with_google_fonts = true
))]
fn get_redoc_html(
    py: Python<'_>,
    openapi_url: &str,
    title: &str,
    redoc_js_url: &str,
    redoc_favicon_url: &str,
    with_google_fonts: bool,
) -> PyResult<Py<PyAny>> {
    html_response(
        py,
        redoc_page(
            openapi_url,
            title,
            redoc_js_url,
            redoc_favicon_url,
            with_google_fonts,
        ),
    )
}

/// Generate the HTML response with the OAuth2 redirection for Swagger UI.
///
/// You normally don't need to use or change this.
#[pyfunction]
fn get_swagger_ui_oauth2_redirect_html(py: Python<'_>) -> PyResult<Py<PyAny>> {
    html_response(py, OAUTH2_REDIRECT_HTML.to_owned())
}

pub(crate) fn register(docs: &Bound<'_, PyModule>) -> PyResult<()> {
    docs.add_function(wrap_pyfunction!(get_swagger_ui_html, docs)?)?;
    docs.add_function(wrap_pyfunction!(get_redoc_html, docs)?)?;
    docs.add_function(wrap_pyfunction!(get_swagger_ui_oauth2_redirect_html, docs)?)?;
    docs.add(
        "swagger_ui_default_parameters",
        swagger_ui_default_parameters(docs.py())?,
    )
}
