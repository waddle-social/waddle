//! Message enrichment: detects GitHub URLs and appends metadata elements.
//!
//! The enricher is designed to be called **once before fan-out** in the message
//! pipeline — before carbons, MAM archival, and routing. This ensures all
//! recipients (including carbon copies) see the same enriched message.
//!
//! Enrichment is **fail-open**: if the GitHub API is unreachable, the message
//! is delivered without embeds. No message is ever blocked by enrichment.

use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, instrument};
use xmpp_parsers::message::Message;

use crate::client::GitHubClient;
use crate::detect::{detect_github_links, GitHubLink};
use crate::embed::*;
use crate::MAX_LINKS_PER_MESSAGE;

/// Type alias for an optional built embed element.
type MaybeElement = Option<minidom::Element>;

/// Message enricher that wraps a `GitHubClient` and can be shared across connections.
///
/// Thread-safe via `Arc`; the inner `GitHubClient` uses `Mutex`-protected caches.
pub struct MessageEnricher {
    client: Arc<GitHubClient>,
    enabled: bool,
}

impl MessageEnricher {
    /// Create a new enricher.
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self {
            client,
            enabled: true,
        }
    }

    /// Create from environment. Reads `GITHUB_TOKEN` and `WADDLE_GITHUB_ENRICH`
    /// (set to `false` or `0` to disable).
    pub fn from_env() -> Self {
        let enabled = std::env::var("WADDLE_GITHUB_ENRICH")
            .map(|v| !matches!(v.as_str(), "false" | "0" | "no"))
            .unwrap_or(true);

        let client = Arc::new(GitHubClient::from_env());

        if !enabled {
            debug!("GitHub message enrichment is disabled");
        }

        Self { client, enabled }
    }

    /// Returns `true` if enrichment is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enrich a message in-place by detecting GitHub URLs in the body and
    /// appending metadata elements.
    ///
    /// Returns the number of embeds added (0 if none or on error).
    ///
    /// This method is fail-open: errors are logged but never propagated.
    #[instrument(skip(self, msg), fields(embeds_added = tracing::field::Empty))]
    pub async fn enrich_message(&self, msg: &mut Message) -> usize {
        if !self.enabled {
            return 0;
        }

        // Don't re-enrich messages that already have GitHub embeds
        if message_has_github_embed(msg) {
            debug!("Message already has GitHub embeds, skipping enrichment");
            return 0;
        }

        // Extract body text
        let body = match msg.bodies.get("").or_else(|| msg.bodies.values().next()) {
            Some(body) => body.0.clone(),
            None => return 0,
        };

        // Detect GitHub links
        let links = detect_github_links(&body, MAX_LINKS_PER_MESSAGE);
        if links.is_empty() {
            return 0;
        }

        debug!(link_count = links.len(), "Detected GitHub links in message");
        let start = Instant::now();

        // Fetch all links concurrently to minimize latency.
        // Worst case with 3 links is one round of parallel HTTP calls (~3s)
        // instead of 6 sequential calls (~18s).
        let futures: Vec<_> = links
            .iter()
            .map(|link| self.build_embed_for_link(link))
            .collect();
        let results = futures::future::join_all(futures).await;

        let mut embeds_added = 0;
        for element in results.into_iter().flatten() {
            msg.payloads.push(element);
            embeds_added += 1;
        }

        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        if embeds_added > 0 {
            debug!(
                embeds_added,
                elapsed_ms = format!("{:.1}", elapsed_ms),
                "GitHub enrichment complete"
            );
        }

        embeds_added
    }

    /// Dispatch to the appropriate embed builder based on link type.
    async fn build_embed_for_link(&self, link: &GitHubLink) -> MaybeElement {
        match link {
            GitHubLink::Repo { owner, repo } => self.build_repo_embed(owner, repo).await,
            GitHubLink::Issue { owner, repo, number } => {
                self.build_issue_embed(owner, repo, *number).await
            }
            GitHubLink::PullRequest { owner, repo, number } => {
                self.build_pr_embed(owner, repo, *number).await
            }
        }
    }

    /// Build a `<repo>` embed element by fetching repo + languages concurrently.
    async fn build_repo_embed(
        &self,
        owner: &str,
        repo: &str,
    ) -> Option<minidom::Element> {
        // Fetch repo metadata and languages in parallel
        let (repo_info, languages) = tokio::join!(
            self.client.fetch_repo(owner, repo),
            self.client.fetch_languages(owner, repo),
        );
        let repo_info = repo_info?;

        let mut embed = GitHubRepoEmbed::new(
            format!("https://github.com/{owner}/{repo}"),
            owner,
            repo_info.full_name.split('/').next_back().unwrap_or(repo),
        );

        embed.description = repo_info.description;
        embed.stars = Some(repo_info.stargazers_count);
        embed.forks = Some(repo_info.forks_count);
        embed.default_branch = Some(repo_info.default_branch);
        embed.topics = repo_info.topics;
        embed.license = repo_info
            .license
            .and_then(|l| l.spdx_id.filter(|id| id != "NOASSERTION").or(Some(l.name)));
        embed.languages = languages
            .into_iter()
            .map(|(name, bytes)| Language { name, bytes })
            .collect();

        Some(build_repo_element(&embed))
    }

    /// Build an `<issue>` embed element.
    async fn build_issue_embed(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Option<minidom::Element> {
        let info = self.client.fetch_issue(owner, repo, number).await?;

        let mut embed = GitHubIssueEmbed::new(
            format!("https://github.com/{owner}/{repo}/issues/{number}"),
            format!("{owner}/{repo}"),
            number.to_string(),
            &info.title,
            &info.user.login,
        );

        embed.state = Some(info.state);
        embed.assignee = info.assignee.map(|a| a.login);
        embed.labels = info.labels.into_iter().map(|l| l.name).collect();

        Some(build_issue_element(&embed))
    }

    /// Build a `<pr>` embed element.
    async fn build_pr_embed(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Option<minidom::Element> {
        let info = self.client.fetch_pull_request(owner, repo, number).await?;

        let mut embed = GitHubPullRequestEmbed::new(
            format!("https://github.com/{owner}/{repo}/pull/{number}"),
            format!("{owner}/{repo}"),
            number.to_string(),
            &info.title,
            &info.user.login,
        );

        embed.state = Some(info.state);
        embed.draft = info.draft;
        embed.merged = info.merged;
        embed.base = info.base.map(|b| b.ref_name);
        embed.head = info.head.map(|h| h.ref_name);
        embed.labels = info.labels.into_iter().map(|l| l.name).collect();

        Some(build_pr_element(&embed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_enricher_disabled() {
        let client = Arc::new(GitHubClient::new(None));
        let mut enricher = MessageEnricher::new(client);
        enricher.enabled = false;

        let mut msg = Message::new(None);
        let count = enricher.enrich_message(&mut msg).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_enricher_no_body() {
        let client = Arc::new(GitHubClient::new(None));
        let enricher = MessageEnricher::new(client);

        let mut msg = Message::new(None);
        let count = enricher.enrich_message(&mut msg).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_enricher_no_github_links() {
        let client = Arc::new(GitHubClient::new(None));
        let enricher = MessageEnricher::new(client);

        let mut msg = Message::new(None);
        msg.bodies
            .insert(String::new(), xmpp_parsers::message::Body("Hello world".into()));
        let count = enricher.enrich_message(&mut msg).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_enricher_skip_already_enriched() {
        let client = Arc::new(GitHubClient::new(None));
        let enricher = MessageEnricher::new(client);

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("https://github.com/a/b".into()),
        );

        // Add a fake embed
        let embed = GitHubRepoEmbed::new("https://github.com/a/b", "a", "b");
        msg.payloads.push(build_repo_element(&embed));

        let count = enricher.enrich_message(&mut msg).await;
        assert_eq!(count, 0);
    }
}
