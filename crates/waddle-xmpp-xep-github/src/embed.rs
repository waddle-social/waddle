//! XML embed structs for GitHub metadata.
//!
//! Defines `GitHubRepoEmbed`, `GitHubIssueEmbed`, and `GitHubPullRequestEmbed`
//! with parse/build functions for the `urn:waddle:github:0` namespace.

use minidom::Element;
use tracing::debug;

use crate::NS_WADDLE_GITHUB;

// ============================================================================
// Repository embed
// ============================================================================

/// GitHub repository metadata embedded in a message stanza.
///
/// ```xml
/// <repo xmlns='urn:waddle:github:0'
///       url='https://github.com/owner/repo'
///       owner='owner'
///       name='repo'>
///   <description>A description</description>
///   <language name='Rust' bytes='123456'/>
///   <stars>1000</stars>
///   <forks>100</forks>
///   <default-branch>main</default-branch>
///   <topic>rust</topic>
///   <license>MIT</license>
/// </repo>
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepoEmbed {
    /// Canonical URL.
    pub url: String,
    /// Repository owner (user or org).
    pub owner: String,
    /// Repository name.
    pub name: String,
    /// Description (may be empty).
    pub description: Option<String>,
    /// Languages with byte counts.
    pub languages: Vec<Language>,
    /// Star count.
    pub stars: Option<u64>,
    /// Fork count.
    pub forks: Option<u64>,
    /// Default branch name.
    pub default_branch: Option<String>,
    /// Topics/tags.
    pub topics: Vec<String>,
    /// License SPDX identifier or name.
    pub license: Option<String>,
}

/// A programming language with its byte count in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Language {
    pub name: String,
    pub bytes: u64,
}

impl GitHubRepoEmbed {
    pub fn new(url: impl Into<String>, owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            owner: owner.into(),
            name: name.into(),
            description: None,
            languages: Vec::new(),
            stars: None,
            forks: None,
            default_branch: None,
            topics: Vec::new(),
            license: None,
        }
    }
}

/// Build a `<repo>` element from a `GitHubRepoEmbed`.
pub fn build_repo_element(embed: &GitHubRepoEmbed) -> Element {
    let mut repo = Element::builder("repo", NS_WADDLE_GITHUB)
        .attr("url", &embed.url)
        .attr("owner", &embed.owner)
        .attr("name", &embed.name)
        .build();

    if let Some(ref desc) = embed.description {
        repo.append_child(
            Element::builder("description", NS_WADDLE_GITHUB)
                .append(desc.clone())
                .build(),
        );
    }

    for lang in &embed.languages {
        repo.append_child(
            Element::builder("language", NS_WADDLE_GITHUB)
                .attr("name", &lang.name)
                .attr("bytes", lang.bytes.to_string())
                .build(),
        );
    }

    if let Some(stars) = embed.stars {
        repo.append_child(
            Element::builder("stars", NS_WADDLE_GITHUB)
                .append(stars.to_string())
                .build(),
        );
    }

    if let Some(forks) = embed.forks {
        repo.append_child(
            Element::builder("forks", NS_WADDLE_GITHUB)
                .append(forks.to_string())
                .build(),
        );
    }

    if let Some(ref branch) = embed.default_branch {
        repo.append_child(
            Element::builder("default-branch", NS_WADDLE_GITHUB)
                .append(branch.clone())
                .build(),
        );
    }

    for topic in &embed.topics {
        repo.append_child(
            Element::builder("topic", NS_WADDLE_GITHUB)
                .append(topic.clone())
                .build(),
        );
    }

    if let Some(ref license) = embed.license {
        repo.append_child(
            Element::builder("license", NS_WADDLE_GITHUB)
                .append(license.clone())
                .build(),
        );
    }

    repo
}

/// Parse a `<repo>` element into a `GitHubRepoEmbed`.
pub fn parse_repo_element(element: &Element) -> Option<GitHubRepoEmbed> {
    if element.name() != "repo" || element.ns() != NS_WADDLE_GITHUB {
        return None;
    }

    let url = element.attr("url")?.to_string();
    let owner = element.attr("owner")?.to_string();
    let name = element.attr("name")?.to_string();

    let description = element
        .get_child("description", NS_WADDLE_GITHUB)
        .map(|e| e.text())
        .filter(|s| !s.is_empty());

    let languages = element
        .children()
        .filter(|c| c.name() == "language" && c.ns() == NS_WADDLE_GITHUB)
        .filter_map(|c| {
            let lang_name = c.attr("name")?.to_string();
            let bytes: u64 = c.attr("bytes")?.parse().ok()?;
            Some(Language {
                name: lang_name,
                bytes,
            })
        })
        .collect();

    let stars = element
        .get_child("stars", NS_WADDLE_GITHUB)
        .and_then(|e| e.text().parse().ok());

    let forks = element
        .get_child("forks", NS_WADDLE_GITHUB)
        .and_then(|e| e.text().parse().ok());

    let default_branch = element
        .get_child("default-branch", NS_WADDLE_GITHUB)
        .map(|e| e.text())
        .filter(|s| !s.is_empty());

    let topics = element
        .children()
        .filter(|c| c.name() == "topic" && c.ns() == NS_WADDLE_GITHUB)
        .map(|c| c.text())
        .filter(|s| !s.is_empty())
        .collect();

    let license = element
        .get_child("license", NS_WADDLE_GITHUB)
        .map(|e| e.text())
        .filter(|s| !s.is_empty());

    debug!(owner = %owner, name = %name, "Parsed GitHub repo embed");

    Some(GitHubRepoEmbed {
        url,
        owner,
        name,
        description,
        languages,
        stars,
        forks,
        default_branch,
        topics,
        license,
    })
}

// ============================================================================
// Issue embed
// ============================================================================

/// GitHub issue metadata embedded in a message stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubIssueEmbed {
    /// Canonical issue URL.
    pub url: String,
    /// Repository in "owner/repo" form.
    pub repo: String,
    /// Issue number.
    pub number: String,
    /// Issue state (e.g., "open", "closed").
    pub state: Option<String>,
    /// Issue title.
    pub title: String,
    /// Issue author username.
    pub author: String,
    /// Optional assignee username.
    pub assignee: Option<String>,
    /// Labels.
    pub labels: Vec<String>,
}

impl GitHubIssueEmbed {
    pub fn new(
        url: impl Into<String>,
        repo: impl Into<String>,
        number: impl Into<String>,
        title: impl Into<String>,
        author: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            repo: repo.into(),
            number: number.into(),
            state: None,
            title: title.into(),
            author: author.into(),
            assignee: None,
            labels: Vec::new(),
        }
    }
}

/// Build an `<issue>` element.
pub fn build_issue_element(embed: &GitHubIssueEmbed) -> Element {
    let mut builder = Element::builder("issue", NS_WADDLE_GITHUB)
        .attr("url", &embed.url)
        .attr("repo", &embed.repo)
        .attr("number", &embed.number);

    if let Some(ref state) = embed.state {
        builder = builder.attr("state", state.as_str());
    }

    let mut issue = builder.build();

    issue.append_child(
        Element::builder("title", NS_WADDLE_GITHUB)
            .append(embed.title.clone())
            .build(),
    );
    issue.append_child(
        Element::builder("author", NS_WADDLE_GITHUB)
            .append(embed.author.clone())
            .build(),
    );

    if let Some(ref assignee) = embed.assignee {
        issue.append_child(
            Element::builder("assignee", NS_WADDLE_GITHUB)
                .append(assignee.clone())
                .build(),
        );
    }

    if !embed.labels.is_empty() {
        let mut labels_elem = Element::builder("labels", NS_WADDLE_GITHUB).build();
        for label in &embed.labels {
            labels_elem.append_child(
                Element::builder("label", NS_WADDLE_GITHUB)
                    .append(label.clone())
                    .build(),
            );
        }
        issue.append_child(labels_elem);
    }

    issue
}

/// Parse an `<issue>` element.
pub fn parse_issue_element(element: &Element) -> Option<GitHubIssueEmbed> {
    if element.name() != "issue" || element.ns() != NS_WADDLE_GITHUB {
        return None;
    }

    let url = element.attr("url")?.to_string();
    let repo = element.attr("repo")?.to_string();
    let number = element.attr("number")?.to_string();
    let state = element.attr("state").map(|s| s.to_string());

    let title = element.get_child("title", NS_WADDLE_GITHUB)?.text();
    let author = element.get_child("author", NS_WADDLE_GITHUB)?.text();
    let assignee = element
        .get_child("assignee", NS_WADDLE_GITHUB)
        .map(|e| e.text())
        .filter(|s| !s.is_empty());

    let labels = element
        .get_child("labels", NS_WADDLE_GITHUB)
        .map(|labels_elem| {
            labels_elem
                .children()
                .filter(|child| child.name() == "label" && child.ns() == NS_WADDLE_GITHUB)
                .map(|label| label.text())
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    debug!(repo = %repo, number = %number, "Parsed GitHub issue embed");

    Some(GitHubIssueEmbed {
        url,
        repo,
        number,
        state,
        title,
        author,
        assignee,
        labels,
    })
}

// ============================================================================
// Pull Request embed
// ============================================================================

/// GitHub pull request metadata embedded in a message stanza.
///
/// ```xml
/// <pr xmlns='urn:waddle:github:0'
///     url='https://github.com/owner/repo/pull/42'
///     repo='owner/repo'
///     number='42'
///     state='open'
///     draft='false'
///     merged='false'>
///   <title>Add feature X</title>
///   <author>octocat</author>
///   <base>main</base>
///   <head>feature-x</head>
///   <labels>
///     <label>enhancement</label>
///   </labels>
/// </pr>
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestEmbed {
    /// Canonical PR URL.
    pub url: String,
    /// Repository in "owner/repo" form.
    pub repo: String,
    /// PR number.
    pub number: String,
    /// PR state (e.g., "open", "closed").
    pub state: Option<String>,
    /// Whether this PR is a draft.
    pub draft: Option<bool>,
    /// Whether this PR has been merged.
    pub merged: Option<bool>,
    /// PR title.
    pub title: String,
    /// PR author username.
    pub author: String,
    /// Base branch (merge target).
    pub base: Option<String>,
    /// Head branch (source).
    pub head: Option<String>,
    /// Labels.
    pub labels: Vec<String>,
}

impl GitHubPullRequestEmbed {
    pub fn new(
        url: impl Into<String>,
        repo: impl Into<String>,
        number: impl Into<String>,
        title: impl Into<String>,
        author: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            repo: repo.into(),
            number: number.into(),
            state: None,
            draft: None,
            merged: None,
            title: title.into(),
            author: author.into(),
            base: None,
            head: None,
            labels: Vec::new(),
        }
    }
}

/// Build a `<pr>` element.
pub fn build_pr_element(embed: &GitHubPullRequestEmbed) -> Element {
    let mut builder = Element::builder("pr", NS_WADDLE_GITHUB)
        .attr("url", &embed.url)
        .attr("repo", &embed.repo)
        .attr("number", &embed.number);

    if let Some(ref state) = embed.state {
        builder = builder.attr("state", state.as_str());
    }
    if let Some(draft) = embed.draft {
        builder = builder.attr("draft", if draft { "true" } else { "false" });
    }
    if let Some(merged) = embed.merged {
        builder = builder.attr("merged", if merged { "true" } else { "false" });
    }

    let mut pr = builder.build();

    pr.append_child(
        Element::builder("title", NS_WADDLE_GITHUB)
            .append(embed.title.clone())
            .build(),
    );
    pr.append_child(
        Element::builder("author", NS_WADDLE_GITHUB)
            .append(embed.author.clone())
            .build(),
    );

    if let Some(ref base) = embed.base {
        pr.append_child(
            Element::builder("base", NS_WADDLE_GITHUB)
                .append(base.clone())
                .build(),
        );
    }
    if let Some(ref head) = embed.head {
        pr.append_child(
            Element::builder("head", NS_WADDLE_GITHUB)
                .append(head.clone())
                .build(),
        );
    }

    if !embed.labels.is_empty() {
        let mut labels_elem = Element::builder("labels", NS_WADDLE_GITHUB).build();
        for label in &embed.labels {
            labels_elem.append_child(
                Element::builder("label", NS_WADDLE_GITHUB)
                    .append(label.clone())
                    .build(),
            );
        }
        pr.append_child(labels_elem);
    }

    pr
}

/// Parse a `<pr>` element.
pub fn parse_pr_element(element: &Element) -> Option<GitHubPullRequestEmbed> {
    if element.name() != "pr" || element.ns() != NS_WADDLE_GITHUB {
        return None;
    }

    let url = element.attr("url")?.to_string();
    let repo = element.attr("repo")?.to_string();
    let number = element.attr("number")?.to_string();
    let state = element.attr("state").map(|s| s.to_string());
    let draft = element.attr("draft").map(|s| s == "true");
    let merged = element.attr("merged").map(|s| s == "true");

    let title = element.get_child("title", NS_WADDLE_GITHUB)?.text();
    let author = element.get_child("author", NS_WADDLE_GITHUB)?.text();

    let base = element
        .get_child("base", NS_WADDLE_GITHUB)
        .map(|e| e.text())
        .filter(|s| !s.is_empty());
    let head = element
        .get_child("head", NS_WADDLE_GITHUB)
        .map(|e| e.text())
        .filter(|s| !s.is_empty());

    let labels = element
        .get_child("labels", NS_WADDLE_GITHUB)
        .map(|labels_elem| {
            labels_elem
                .children()
                .filter(|child| child.name() == "label" && child.ns() == NS_WADDLE_GITHUB)
                .map(|label| label.text())
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    debug!(repo = %repo, number = %number, "Parsed GitHub PR embed");

    Some(GitHubPullRequestEmbed {
        url,
        repo,
        number,
        state,
        draft,
        merged,
        title,
        author,
        base,
        head,
        labels,
    })
}

/// Check if a message already contains any GitHub embed elements.
pub fn message_has_github_embed(msg: &xmpp_parsers::message::Message) -> bool {
    msg.payloads.iter().any(|p| {
        p.ns() == NS_WADDLE_GITHUB
            && (p.name() == "repo" || p.name() == "issue" || p.name() == "pr")
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repo_roundtrip() {
        let mut embed = GitHubRepoEmbed::new(
            "https://github.com/rust-lang/rust",
            "rust-lang",
            "rust",
        );
        embed.description = Some("The Rust programming language".into());
        embed.languages = vec![
            Language { name: "Rust".into(), bytes: 100_000 },
            Language { name: "Python".into(), bytes: 5_000 },
        ];
        embed.stars = Some(100_000);
        embed.forks = Some(12_000);
        embed.default_branch = Some("master".into());
        embed.topics = vec!["rust".into(), "programming-language".into()];
        embed.license = Some("MIT".into());

        let element = build_repo_element(&embed);
        let parsed = parse_repo_element(&element).unwrap();
        assert_eq!(embed, parsed);
    }

    #[test]
    fn test_repo_minimal() {
        let embed = GitHubRepoEmbed::new(
            "https://github.com/a/b",
            "a",
            "b",
        );
        let element = build_repo_element(&embed);
        let parsed = parse_repo_element(&element).unwrap();
        assert_eq!(embed, parsed);
    }

    #[test]
    fn test_issue_roundtrip() {
        let mut embed = GitHubIssueEmbed::new(
            "https://github.com/owner/repo/issues/42",
            "owner/repo",
            "42",
            "Bug report",
            "octocat",
        );
        embed.state = Some("open".into());
        embed.assignee = Some("hubot".into());
        embed.labels = vec!["bug".into(), "critical".into()];

        let element = build_issue_element(&embed);
        let parsed = parse_issue_element(&element).unwrap();
        assert_eq!(embed, parsed);
    }

    #[test]
    fn test_issue_missing_required() {
        let xml = r#"<issue xmlns='urn:waddle:github:0' url='https://github.com/a/b/issues/1'>
            <title>Missing</title>
        </issue>"#;
        let element: Element = xml.parse().unwrap();
        assert!(parse_issue_element(&element).is_none());
    }

    #[test]
    fn test_pr_roundtrip() {
        let mut embed = GitHubPullRequestEmbed::new(
            "https://github.com/owner/repo/pull/99",
            "owner/repo",
            "99",
            "Add feature X",
            "contributor",
        );
        embed.state = Some("open".into());
        embed.draft = Some(true);
        embed.merged = Some(false);
        embed.base = Some("main".into());
        embed.head = Some("feature-x".into());
        embed.labels = vec!["enhancement".into()];

        let element = build_pr_element(&embed);
        let parsed = parse_pr_element(&element).unwrap();
        assert_eq!(embed, parsed);
    }

    #[test]
    fn test_pr_minimal() {
        let embed = GitHubPullRequestEmbed::new(
            "https://github.com/a/b/pull/1",
            "a/b",
            "1",
            "Title",
            "author",
        );
        let element = build_pr_element(&embed);
        let parsed = parse_pr_element(&element).unwrap();
        assert_eq!(embed, parsed);
    }

    #[test]
    fn test_wrong_element_name() {
        let xml = r#"<other xmlns='urn:waddle:github:0' url='x' owner='a' name='b'/>"#;
        let element: Element = xml.parse().unwrap();
        assert!(parse_repo_element(&element).is_none());
    }

    #[test]
    fn test_wrong_namespace() {
        let xml = r#"<repo xmlns='urn:other:ns' url='x' owner='a' name='b'/>"#;
        let element: Element = xml.parse().unwrap();
        assert!(parse_repo_element(&element).is_none());
    }
}
