use regex::{Captures, Regex};
use std::sync::LazyLock;

static PATH_PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([^}]+)\}").expect("literal regex is valid"));

fn param_name(placeholder: &str) -> &str {
    placeholder
        .split_once(':')
        .map_or(placeholder, |(name, _)| name)
}

pub fn extract_path_param_names(path: &str) -> Vec<String> {
    PATH_PARAM
        .captures_iter(path)
        .map(|caps| param_name(&caps[1]).to_owned())
        .collect()
}

pub fn strip_path_convertors(path: &str) -> String {
    PATH_PARAM
        .replace_all(path, |caps: &Captures| {
            format!("{{{}}}", param_name(&caps[1]))
        })
        .into_owned()
}

#[must_use]
pub fn join_path(a: &str, b: &str) -> String {
    let a_ends = a.ends_with('/');
    let b_starts = b.starts_with('/');

    let capacity = match (a_ends, b_starts) {
        (true, true) => a.len() + b.len() - 1,
        (false, false) => a.len() + b.len() + 1,
        _ => a.len() + b.len(),
    };

    let mut path = String::with_capacity(capacity);
    path.push_str(a);

    match (a_ends, b_starts) {
        (true, true) => {
            path.pop();
            path.push_str(b);
        }
        (false, false) => {
            path.push('/');
            path.push_str(b);
        }
        _ => {
            path.push_str(b);
        }
    }
    path
}

#[must_use]
pub fn strip_root_path<'a>(root: &str, original_path: &'a str) -> Option<&'a str> {
    if root.is_empty() {
        Some(original_path)
    } else if original_path == root {
        Some("/")
    } else if let Some(stripped) = original_path.strip_prefix(root) {
        stripped.starts_with('/').then_some(stripped)
    } else {
        None
    }
}
