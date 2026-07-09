//! Immutable source identity shared by every server-side XMPP surface.

/// Sentinel exposed by local/custom builds that did not provide a full source
/// revision at compile time. Gate evidence rejects this value.
pub const UNKNOWN_BUILD_COMMIT: &str = "unknown";

/// Return the full lowercase Git SHA embedded by the package build.
pub fn embedded_git_sha() -> Option<&'static str> {
    option_env!("WADDLE_BUILD_GIT_SHA").filter(|value| is_full_git_sha(value))
}

/// Printable identity used by diagnostics and XEP-0092.
pub fn printable_git_sha() -> &'static str {
    embedded_git_sha().unwrap_or(UNKNOWN_BUILD_COMMIT)
}

fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_full_lowercase_git_shas() {
        assert!(is_full_git_sha("0123456789abcdef0123456789abcdef01234567"));
        for invalid in [
            "unknown",
            "01234567",
            "0123456789ABCDEF0123456789ABCDEF01234567",
            "g123456789abcdef0123456789abcdef01234567",
        ] {
            assert!(!is_full_git_sha(invalid));
        }
    }
}
