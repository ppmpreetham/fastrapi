use std::sync::OnceLock;

pub fn extract_path_param_names(path: &str) -> Vec<String> {
    static PATH_PARAM_REGEX: OnceLock<regex::Regex> = OnceLock::new();
    let regex = PATH_PARAM_REGEX
        .get_or_init(|| regex::Regex::new(r"\{([^}]+)\}").expect("Invalid path param regex"));
    regex
        .captures_iter(path)
        .map(|caps| caps[1].split(':').next().unwrap_or(&caps[1]).to_string())
        .collect()
}
