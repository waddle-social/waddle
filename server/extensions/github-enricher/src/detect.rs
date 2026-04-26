use regex::Regex;

pub fn github_links(body: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"https?://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:/(issues|pull)/\d+)?")
            .expect("valid regex")
    });

    re.find_iter(body)
        .filter_map(|m| normalize_github_url(m.as_str()))
        .collect()
}

pub fn normalize_github_url(url: &str) -> Option<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^https?://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:/(issues|pull)/\d+)?$")
            .expect("valid regex")
    });

    if !re.is_match(url) {
        return None;
    }

    Some(match url.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{github_links, normalize_github_url};

    #[test]
    fn detects_repo_and_issue_links() {
        let links = github_links("repo https://github.com/rust-lang/rust and issue https://github.com/rust-lang/rust/issues/1");
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn normalizes_http_links_to_https() {
        let links = github_links("repo http://github.com/waddle-social/waddle");
        assert_eq!(links, vec!["https://github.com/waddle-social/waddle"]);
    }

    #[test]
    fn rejects_non_github_urls() {
        assert_eq!(
            normalize_github_url("https://evil.example/waddle-social/waddle"),
            None
        );
        assert_eq!(
            normalize_github_url("https://github.com.evil.example/waddle-social/waddle"),
            None
        );
    }
}
