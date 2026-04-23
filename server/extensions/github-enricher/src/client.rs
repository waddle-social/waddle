use crate::EmbedElement;

/// Parse owner and repo name from a GitHub URL path.
/// Expects URLs validated by `detect::github_links` (e.g. `https://github.com/owner/repo/...`).
fn parse_owner_repo(url: &str) -> (String, String) {
    let path = url.split("github.com/").nth(1).unwrap_or("");
    let mut parts = path.split('/');
    let owner = parts.next().unwrap_or("").to_string();
    let name = parts.next().unwrap_or("").to_string();
    (owner, name)
}

/// Determine the embed element name from the URL path segment after owner/repo.
fn embed_element_name(url: &str) -> &'static str {
    let path = url.split("github.com/").nth(1).unwrap_or("");
    let segment = path.split('/').nth(2).unwrap_or("");
    match segment {
        "issues" => "issue",
        "pull" => "pr",
        _ => "repo",
    }
}

pub fn build_repo_embed(url: &str) -> EmbedElement {
    let (owner, name) = parse_owner_repo(url);
    let element_name = embed_element_name(url);

    EmbedElement {
        element_name: element_name.to_string(),
        namespace: "urn:waddle:github:0".to_string(),
        attributes: vec![
            ("url".to_string(), url.to_string()),
            ("owner".to_string(), owner),
            ("name".to_string(), name),
        ],
        children: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_url() {
        let embed = build_repo_embed("https://github.com/waddle-social/waddle");
        assert_eq!(embed.element_name, "repo");
        assert_eq!(embed.attributes[1].1, "waddle-social");
        assert_eq!(embed.attributes[2].1, "waddle");
    }

    #[test]
    fn parse_issue_url() {
        let embed = build_repo_embed("https://github.com/waddle-social/waddle/issues/42");
        assert_eq!(embed.element_name, "issue");
    }

    #[test]
    fn parse_pr_url() {
        let embed = build_repo_embed("https://github.com/waddle-social/waddle/pull/48");
        assert_eq!(embed.element_name, "pr");
    }
}
