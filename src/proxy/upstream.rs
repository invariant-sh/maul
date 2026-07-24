pub fn build_upstream_url(upstream_base_url: &str, path_and_query: &str) -> String {
    let base = upstream_base_url.trim_end_matches('/');
    let path = if path_and_query.is_empty() {
        "/"
    } else {
        path_and_query
    };
    format!("{base}{path}")
}
