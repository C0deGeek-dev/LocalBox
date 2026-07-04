//! The LAN serve-gateway guard.
//!
//! The serve guard refuses the one genuinely dangerous shape — open (no-auth)
//! HTTP on a public-looking address — unless the operator opts in explicitly;
//! everything else is visible, not blocked.

/// Whether a base URL is *public-looking* plain HTTP: `http://` on anything
/// that is not loopback, RFC-1918/link-local private space, or `localhost`.
/// HTTPS is never flagged (transport is protected); unparseable URLs are not
/// flagged here (they fail elsewhere).
#[must_use]
pub fn is_public_http(base_url: &str) -> bool {
    let Some(rest) = base_url.strip_prefix("http://") else {
        return false; // https:// or not a URL — not the open-HTTP hazard
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip credentials and port; tolerate a bracketed IPv6 literal.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                let o = v4.octets();
                !(o[0] == 10
                    || o[0] == 127
                    || (o[0] == 192 && o[1] == 168)
                    || (o[0] == 172 && (16..=31).contains(&o[1]))
                    || (o[0] == 169 && o[1] == 254))
            }
            std::net::IpAddr::V6(v6) => !v6.is_loopback(),
        };
    }
    // A DNS name over plain HTTP: treat as public-looking.
    true
}

/// The serve-gateway exposure decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeGuard {
    /// The base URLs that look publicly reachable over plain HTTP.
    pub public_urls: Vec<String>,
    /// The launch must be refused (open no-auth HTTP on a public address,
    /// with no explicit opt-in).
    pub refuse: bool,
    /// The operator explicitly opted into open public HTTP.
    pub opted_in: bool,
    /// The operator-facing refusal reason; empty when allowed.
    pub reason: String,
}

/// Decide the serve-gateway exposure: open (no-auth) HTTP on a public-looking
/// address is refused unless explicitly opted into; password-protected public
/// HTTP stays allowed.
#[must_use]
pub fn evaluate_serve_guard(
    base_urls: &[String],
    password: &str,
    allow_public_no_auth: bool,
) -> ServeGuard {
    let public_urls: Vec<String> = base_urls
        .iter()
        .filter(|u| is_public_http(u))
        .cloned()
        .collect();
    let no_auth = password.trim().is_empty();
    let refuse = !public_urls.is_empty() && no_auth && !allow_public_no_auth;
    let reason = if refuse {
        format!(
            "open (no auth) HTTP on a public-looking address: {}. Set a password \
             (--password <key>), keep the gateway off the public network (drop \
             --lan or bind a private address), or opt in explicitly with \
             --allow-public-no-auth.",
            public_urls.join(", ")
        )
    } else {
        String::new()
    };
    ServeGuard {
        opted_in: allow_public_no_auth && no_auth && !public_urls.is_empty(),
        public_urls,
        refuse,
        reason,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_public_http_classifier_spares_private_space() {
        for private in [
            "http://127.0.0.1:11435",
            "http://localhost:8080",
            "http://10.1.2.3:8080",
            "http://192.168.1.20:8080",
            "http://172.16.0.9:8080",
            "http://172.31.255.1:8080",
            "http://169.254.10.10:8080",
            "http://[::1]:8080",
            "https://93.184.216.34:8080",
        ] {
            assert!(!is_public_http(private), "{private} must not flag");
        }
        for public in [
            "http://93.184.216.34:8080",
            "http://172.32.0.1:8080",
            "http://myhost.example.com:8080",
            "http://[2001:db8::1]:8080",
        ] {
            assert!(is_public_http(public), "{public} must flag");
        }
    }

    #[test]
    fn no_token_public_serve_is_refused_with_the_remedy() {
        let guard = evaluate_serve_guard(
            &[
                "http://192.168.1.20:11436".to_string(),
                "http://93.184.216.34:11436".to_string(),
            ],
            "",
            false,
        );
        assert!(guard.refuse);
        assert_eq!(guard.public_urls, vec!["http://93.184.216.34:11436"]);
        assert!(guard.reason.contains("--password"));
        assert!(guard.reason.contains("--allow-public-no-auth"));
    }

    #[test]
    fn a_token_or_an_explicit_opt_in_allows_the_gateway() {
        let urls = vec!["http://93.184.216.34:11436".to_string()];
        // Password-protected public HTTP: allowed (visible, not blocked).
        let guard = evaluate_serve_guard(&urls, "s3cret", false);
        assert!(!guard.refuse);
        assert!(!guard.opted_in);
        // Explicit opt-in to open public HTTP: allowed and marked.
        let guard = evaluate_serve_guard(&urls, "", true);
        assert!(!guard.refuse);
        assert!(guard.opted_in);
        // Private-only, no auth: nothing to refuse.
        let guard = evaluate_serve_guard(&["http://192.168.1.2:1".to_string()], "", false);
        assert!(!guard.refuse);
        assert!(guard.public_urls.is_empty());
    }
}
