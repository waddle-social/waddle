//! GitHub URL detection in message body text.
//!
//! Extracts GitHub links from plain text, skipping URLs inside markdown
//! fenced code blocks (``` ... ```) and inline code (` ... `).

use regex::Regex;
use std::sync::OnceLock;

/// A detected GitHub link with its type and components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubLink {
    /// A repository link: `github.com/{owner}/{repo}`
    Repo { owner: String, repo: String },
    /// An issue link: `github.com/{owner}/{repo}/issues/{number}`
    Issue {
        owner: String,
        repo: String,
        number: u64,
    },
    /// A pull request link: `github.com/{owner}/{repo}/pull/{number}`
    PullRequest {
        owner: String,
        repo: String,
        number: u64,
    },
}

impl GitHubLink {
    /// Canonical URL for this link.
    pub fn url(&self) -> String {
        match self {
            GitHubLink::Repo { owner, repo } => {
                format!("https://github.com/{owner}/{repo}")
            }
            GitHubLink::Issue {
                owner,
                repo,
                number,
            } => {
                format!("https://github.com/{owner}/{repo}/issues/{number}")
            }
            GitHubLink::PullRequest {
                owner,
                repo,
                number,
            } => {
                format!("https://github.com/{owner}/{repo}/pull/{number}")
            }
        }
    }

    /// Owner of the repository.
    pub fn owner(&self) -> &str {
        match self {
            GitHubLink::Repo { owner, .. }
            | GitHubLink::Issue { owner, .. }
            | GitHubLink::PullRequest { owner, .. } => owner,
        }
    }

    /// Repository name.
    pub fn repo(&self) -> &str {
        match self {
            GitHubLink::Repo { repo, .. }
            | GitHubLink::Issue { repo, .. }
            | GitHubLink::PullRequest { repo, .. } => repo,
        }
    }
}

/// Regex for matching GitHub URLs.
///
/// Matches:
/// - `https://github.com/{owner}/{repo}`
/// - `https://github.com/{owner}/{repo}/issues/{number}`
/// - `https://github.com/{owner}/{repo}/pull/{number}`
///
/// Owner/repo segments must be valid GitHub identifiers (alphanumeric, hyphens, underscores, dots).
/// Does NOT match github.io or other GitHub subdomains.
fn github_url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"https?://github\.com/([A-Za-z0-9_.\-]+)/([A-Za-z0-9_.\-]+)(?:/(issues|pull)/(\d+))?(?:[#?]\S*)?"
        ).expect("GitHub URL regex is valid")
    })
}

/// Strip fenced code blocks and inline code from text, replacing them with spaces
/// so that URLs inside code are not detected.
#[allow(clippy::while_let_on_iterator)] // Intentional: we need Peekable::peek() inside the loop
fn strip_code_blocks(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '`' {
            // Check for fenced code block (```)
            if chars.peek() == Some(&'`') {
                chars.next();
                if chars.peek() == Some(&'`') {
                    chars.next();
                    // Consume everything until closing ```
                    let mut fence_closed = false;
                    while let Some(c) = chars.next() {
                        if c == '`' && chars.peek() == Some(&'`') {
                            chars.next();
                            if chars.peek() == Some(&'`') {
                                chars.next();
                                fence_closed = true;
                                break;
                            }
                            result.push(' ');
                            result.push(' ');
                        }
                        result.push(' ');
                    }
                    if fence_closed {
                        result.push(' ');
                        result.push(' ');
                        result.push(' ');
                    }
                } else {
                    // Two backticks — treat as inline code delimiter ``
                    result.push(' ');
                    result.push(' ');
                    while let Some(c) = chars.next() {
                        if c == '`' && chars.peek() == Some(&'`') {
                            chars.next();
                            result.push(' ');
                            result.push(' ');
                            break;
                        }
                        result.push(' ');
                    }
                }
            } else {
                // Single backtick — inline code
                result.push(' ');
                while let Some(c) = chars.next() {
                    if c == '`' {
                        result.push(' ');
                        break;
                    }
                    result.push(' ');
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Detect GitHub links in a message body, skipping code blocks.
///
/// Returns up to `max_links` unique links. Deduplicates by URL.
pub fn detect_github_links(body: &str, max_links: usize) -> Vec<GitHubLink> {
    let cleaned = strip_code_blocks(body);
    let re = github_url_regex();
    let mut links = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    for cap in re.captures_iter(&cleaned) {
        if links.len() >= max_links {
            break;
        }

        let owner = cap[1].to_string();
        let repo = cap[2].to_string();

        // Skip if owner or repo segment is a reserved GitHub route
        if is_reserved_owner(&owner) || is_reserved_path(&repo) {
            continue;
        }

        // Guard against path traversal segments (e.g. ".." or ".")
        if owner.contains("..") || repo.contains("..") || owner == "." || repo == "." {
            continue;
        }

        let link = match (cap.get(3), cap.get(4)) {
            (Some(kind), Some(num)) => {
                let number: u64 = match num.as_str().parse() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                match kind.as_str() {
                    "issues" => GitHubLink::Issue {
                        owner,
                        repo,
                        number,
                    },
                    "pull" => GitHubLink::PullRequest {
                        owner,
                        repo,
                        number,
                    },
                    _ => continue,
                }
            }
            _ => GitHubLink::Repo { owner, repo },
        };

        let url = link.url();
        if seen_urls.insert(url) {
            links.push(link);
        }
    }

    links
}

/// Check if a path segment is a reserved GitHub route (not a repo name).
///
/// Applies to both owner and repo segments. GitHub reserves certain top-level
/// paths that are not user/org names.
fn is_reserved_path(segment: &str) -> bool {
    matches!(
        segment,
        "settings"
            | "notifications"
            | "login"
            | "logout"
            | "signup"
            | "explore"
            | "marketplace"
            | "pricing"
            | "features"
            | "enterprise"
            | "sponsors"
            | "topics"
            | "collections"
            | "trending"
            | "about"
            | "security"
            | "site"
            | "orgs"
            | "users"
            | "codespaces"
            | "pulls"
            | "issues"
            | "stars"
            | "new"
    )
}

/// Check if the owner segment is a reserved GitHub top-level path.
fn is_reserved_owner(segment: &str) -> bool {
    is_reserved_path(segment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_repo_link() {
        let links = detect_github_links("Check out https://github.com/rust-lang/rust", 3);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0],
            GitHubLink::Repo {
                owner: "rust-lang".into(),
                repo: "rust".into()
            }
        );
    }

    #[test]
    fn test_detect_issue_link() {
        let links =
            detect_github_links("See https://github.com/owner/repo/issues/42 for details", 3);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0],
            GitHubLink::Issue {
                owner: "owner".into(),
                repo: "repo".into(),
                number: 42,
            }
        );
    }

    #[test]
    fn test_detect_pr_link() {
        let links = detect_github_links("Review https://github.com/owner/repo/pull/99", 3);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0],
            GitHubLink::PullRequest {
                owner: "owner".into(),
                repo: "repo".into(),
                number: 99,
            }
        );
    }

    #[test]
    fn test_detect_multiple_links() {
        let body = "https://github.com/a/b and https://github.com/c/d/issues/1";
        let links = detect_github_links(body, 3);
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn test_max_links_cap() {
        let body = "https://github.com/a/b https://github.com/c/d https://github.com/e/f https://github.com/g/h";
        let links = detect_github_links(body, 2);
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn test_dedup_links() {
        let body = "https://github.com/a/b and again https://github.com/a/b";
        let links = detect_github_links(body, 3);
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_skip_code_block() {
        let body = "text ```\nhttps://github.com/a/b\n``` more text";
        let links = detect_github_links(body, 3);
        assert!(
            links.is_empty(),
            "Should not detect links inside fenced code blocks"
        );
    }

    #[test]
    fn test_skip_inline_code() {
        let body = "Use `https://github.com/a/b` for reference";
        let links = detect_github_links(body, 3);
        assert!(
            links.is_empty(),
            "Should not detect links inside inline code"
        );
    }

    #[test]
    fn test_link_outside_code_block_detected() {
        let body = "```\ncode\n``` https://github.com/a/b";
        let links = detect_github_links(body, 3);
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_no_match_github_io() {
        let body = "Visit https://owner.github.io/project for docs";
        let links = detect_github_links(body, 3);
        assert!(links.is_empty(), "Should not match github.io URLs");
    }

    #[test]
    fn test_url_with_fragment() {
        let links = detect_github_links("https://github.com/a/b#readme", 3);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0],
            GitHubLink::Repo {
                owner: "a".into(),
                repo: "b".into()
            }
        );
    }

    #[test]
    fn test_url_with_query_params() {
        let links = detect_github_links("https://github.com/a/b?tab=repositories", 3);
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_reserved_repo_paths_skipped() {
        let body = "https://github.com/user/settings";
        let links = detect_github_links(body, 3);
        assert!(links.is_empty());
    }

    #[test]
    fn test_reserved_owner_paths_skipped() {
        // github.com/orgs/community should not match as a repo
        let body = "https://github.com/orgs/community";
        let links = detect_github_links(body, 3);
        assert!(links.is_empty());
    }

    #[test]
    fn test_reserved_issues_pulls_paths_skipped() {
        // github.com/pulls and github.com/issues are dashboard pages, not repos
        let body = "https://github.com/user/pulls https://github.com/user/issues";
        let links = detect_github_links(body, 3);
        assert!(links.is_empty());
    }

    #[test]
    fn test_http_url() {
        let links = detect_github_links("http://github.com/a/b", 3);
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_no_github_links() {
        let body = "Hello, how are you? Check https://example.com";
        let links = detect_github_links(body, 3);
        assert!(links.is_empty());
    }

    #[test]
    fn test_deep_paths_match_as_repo() {
        // URLs like /tree/main, /blob/main/file.rs still detect the repo — this is
        // intentional since the repo metadata is useful regardless of the sub-path.
        let links = detect_github_links("https://github.com/a/b/tree/main/src", 3);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0],
            GitHubLink::Repo {
                owner: "a".into(),
                repo: "b".into()
            }
        );
    }

    #[test]
    fn test_dots_and_underscores_in_names() {
        let links = detect_github_links("https://github.com/my.org/my_repo.rs", 3);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].owner(), "my.org");
        assert_eq!(links[0].repo(), "my_repo.rs");
    }

    #[test]
    fn test_path_traversal_blocked() {
        let links = detect_github_links("https://github.com/../foo", 3);
        assert!(links.is_empty(), "Should block .. in owner segment");

        let links = detect_github_links("https://github.com/owner/..", 3);
        assert!(links.is_empty(), "Should block .. in repo segment");

        let links = detect_github_links("https://github.com/a..b/repo", 3);
        assert!(links.is_empty(), "Should block segments containing ..");
    }

    #[test]
    fn test_single_dot_segment_blocked() {
        let links = detect_github_links("https://github.com/./repo", 3);
        assert!(links.is_empty(), "Should block . as owner segment");
    }
}
