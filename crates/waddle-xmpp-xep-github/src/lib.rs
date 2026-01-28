//! Waddle GitHub Link Metadata (Custom XEP)
//!
//! Custom extension for embedding GitHub issue metadata in XMPP messages.
//! Clients should also include the plain URL in the message <body> for
//! interoperability with non-Waddle clients.
//!
//! ## XML Format
//!
//! ```xml
//! <issue xmlns='urn:waddle:github:0'
//!        url='https://github.com/owner/repo/issues/123'
//!        repo='owner/repo'
//!        number='123'
//!        state='open'>
//!   <title>Bug: Crash on startup</title>
//!   <author>octocat</author>
//!   <assignee>hubot</assignee>
//!   <labels>
//!     <label>bug</label>
//!     <label>high-priority</label>
//!   </labels>
//! </issue>
//! ```
//!
//! Future extensions can add more children or sibling elements (e.g., <pr/>).

use minidom::Element;
use tracing::debug;

/// Namespace for the Waddle GitHub embed extension.
pub const NS_WADDLE_GITHUB: &str = "urn:waddle:github:0";

/// GitHub Issue metadata embedded in a message stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubIssueEmbed {
    /// Canonical issue URL.
    pub url: String,
    /// Repository in "owner/repo" form.
    pub repo: String,
    /// Issue number as a string to avoid integer parsing issues.
    pub number: String,
    /// Issue state (e.g., "open", "closed").
    pub state: Option<String>,
    /// Issue title.
    pub title: String,
    /// Issue author username/login.
    pub author: String,
    /// Optional assignee username/login.
    pub assignee: Option<String>,
    /// Labels attached to the issue.
    pub labels: Vec<String>,
}

impl GitHubIssueEmbed {
    /// Create a new issue embed with required fields.
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

/// Check if an element is a GitHub issue embed.
pub fn is_github_issue_element(element: &Element) -> bool {
    element.name() == "issue" && element.ns() == NS_WADDLE_GITHUB
}

/// Check if a Message contains a GitHub issue embed.
pub fn message_has_github_issue(msg: &xmpp_parsers::message::Message) -> bool {
    msg.payloads
        .iter()
        .any(|p| p.name() == "issue" && p.ns() == NS_WADDLE_GITHUB)
}

/// Parse a GitHub issue embed from an element.
pub fn parse_github_issue_element(element: &Element) -> Option<GitHubIssueEmbed> {
    if !is_github_issue_element(element) {
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

    debug!(
        repo = %repo,
        number = %number,
        label_count = labels.len(),
        "Parsed GitHub issue embed"
    );

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

/// Parse a GitHub issue embed from a Message.
pub fn parse_github_issue_from_message(
    msg: &xmpp_parsers::message::Message,
) -> Option<GitHubIssueEmbed> {
    msg.payloads
        .iter()
        .find(|p| p.name() == "issue" && p.ns() == NS_WADDLE_GITHUB)
        .and_then(parse_github_issue_element)
}

/// Build a GitHub issue embed element.
pub fn build_github_issue_element(embed: &GitHubIssueEmbed) -> Element {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_issue_element() {
        let xml = r#"
            <issue xmlns='urn:waddle:github:0'
                   url='https://github.com/owner/repo/issues/123'
                   repo='owner/repo'
                   number='123'
                   state='open'>
              <title>Crash on startup</title>
              <author>octocat</author>
              <assignee>hubot</assignee>
              <labels>
                <label>bug</label>
                <label>high-priority</label>
              </labels>
            </issue>
        "#;

        let element: Element = xml.parse().unwrap();
        let embed = parse_github_issue_element(&element).unwrap();

        assert_eq!(embed.url, "https://github.com/owner/repo/issues/123");
        assert_eq!(embed.repo, "owner/repo");
        assert_eq!(embed.number, "123");
        assert_eq!(embed.state.as_deref(), Some("open"));
        assert_eq!(embed.title, "Crash on startup");
        assert_eq!(embed.author, "octocat");
        assert_eq!(embed.assignee.as_deref(), Some("hubot"));
        assert_eq!(embed.labels, vec!["bug", "high-priority"]);
    }

    #[test]
    fn test_build_github_issue_element_roundtrip() {
        let mut embed = GitHubIssueEmbed::new(
            "https://github.com/owner/repo/issues/42",
            "owner/repo",
            "42",
            "Meaning of life",
            "octocat",
        );
        embed.state = Some("closed".to_string());
        embed.assignee = Some("hubot".to_string());
        embed.labels = vec!["question".to_string(), "help wanted".to_string()];

        let element = build_github_issue_element(&embed);
        let parsed = parse_github_issue_element(&element).unwrap();

        assert_eq!(embed, parsed);
    }

    #[test]
    fn test_parse_github_issue_element_missing_required() {
        let xml = r#"
            <issue xmlns='urn:waddle:github:0'
                   url='https://github.com/owner/repo/issues/1'>
              <title>Missing fields</title>
              <author>octocat</author>
            </issue>
        "#;

        let element: Element = xml.parse().unwrap();
        assert!(parse_github_issue_element(&element).is_none());
    }
}
