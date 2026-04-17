use regex::Regex;

pub fn github_links(body: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"https?://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:/(issues|pull)/\d+)?")
            .expect("valid regex")
    });

    re.find_iter(body).map(|m| m.as_str().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::github_links;

    #[test]
    fn detects_repo_and_issue_links() {
        let links = github_links("repo https://github.com/rust-lang/rust and issue https://github.com/rust-lang/rust/issues/1");
        assert_eq!(links.len(), 2);
    }
}
