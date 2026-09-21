//! The canonical spelling of a server URL. Every client keys its keychain
//! entries and vault records by it, so two spellings of one server must
//! normalise to the same string. Pure, so the browser client shares it.

/// Hosts a server moved away from, mapped to where it lives now. A URL on a
/// legacy host normalises to the current one, so vault entries and config
/// written by older builds keep matching the server the build talks to.
/// The old host keeps answering (the website proxies the API paths), so a
/// stale entry still syncs even before it is rewritten.
pub const LEGACY_SERVER_ALIASES: &[(&str, &str)] = &[(
    "https://obsink.spencerjireh.com",
    "https://obsink-api.spencerjireh.com",
)];

/// Normalise a server URL so the same server always maps to the same
/// keychain entry: trimmed, no trailing slash, lowercase scheme+host, and a
/// legacy host replaced by its current one.
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
            if let Some((_, current)) = LEGACY_SERVER_ALIASES
                .iter()
                .find(|(legacy, _)| *legacy == out)
            {
                out = (*current).to_string();
            }
            if !path.is_empty() {
                out.push('/');
                out.push_str(path);
            }
            out
        }
        None => trimmed.to_string(),
    }
}

/// The legacy spellings that normalise to `canonical` (already normalised):
/// where an older build may have stored the bearer for this server.
pub fn legacy_server_urls(canonical: &str) -> Vec<String> {
    LEGACY_SERVER_ALIASES
        .iter()
        .filter(|(_, current)| *current == canonical)
        .map(|(legacy, _)| (*legacy).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{legacy_server_urls, normalize_server_url};

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

    #[test]
    fn a_legacy_host_normalizes_to_the_current_one() {
        assert_eq!(
            normalize_server_url("https://obsink.spencerjireh.com/"),
            "https://obsink-api.spencerjireh.com"
        );
        assert_eq!(
            normalize_server_url("HTTPS://OBSINK.spencerjireh.com/vaults"),
            "https://obsink-api.spencerjireh.com/vaults"
        );
        // Other hosts, and the current host itself, are untouched.
        assert_eq!(
            normalize_server_url("https://obsink-api.spencerjireh.com"),
            "https://obsink-api.spencerjireh.com"
        );
        assert_eq!(
            normalize_server_url("http://obsink.spencerjireh.com"),
            "http://obsink.spencerjireh.com"
        );
    }

    #[test]
    fn legacy_spellings_of_a_server() {
        assert_eq!(
            legacy_server_urls("https://obsink-api.spencerjireh.com"),
            vec!["https://obsink.spencerjireh.com".to_string()]
        );
        assert!(legacy_server_urls("https://example.obsink.test").is_empty());
    }
}
