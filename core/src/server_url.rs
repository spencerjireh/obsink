//! The canonical spelling of a server URL. Every client keys its keychain
//! entries and vault records by it, so two spellings of one server must
//! normalise to the same string. Pure, so the browser client shares it.

/// Normalise a server URL so the same server always maps to the same
/// keychain entry: trimmed, no trailing slash, lowercase scheme+host.
pub fn normalize_server_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    match trimmed.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let mut out = format!(
                "{}://{}",
                scheme.to_ascii_lowercase(),
                host.to_ascii_lowercase()
            );
            if !path.is_empty() {
                out.push('/');
                out.push_str(path);
            }
            out
        }
        None => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_server_url;

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_server_url(" HTTPS://Example.ObSink.test/ "),
            "https://example.obsink.test"
        );
        assert_eq!(
            normalize_server_url("https://x.dev/Path/"),
            "https://x.dev/Path"
        );
    }
}
