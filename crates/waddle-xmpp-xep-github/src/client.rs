//! GitHub REST API client with caching and circuit breaker.
//!
//! Fetches repository, issue, and pull request metadata from the GitHub API.
//! Supports optional token-based authentication for higher rate limits.
//!
//! ## Rate limits
//! - Unauthenticated: 60 requests/hour
//! - Token-authenticated: 5,000 requests/hour
//!
//! ## Circuit breaker
//! After `CIRCUIT_BREAKER_THRESHOLD` consecutive failures, the client enters
//! an open state for `CIRCUIT_BREAKER_COOLDOWN` seconds and returns `None`
//! for all requests without making HTTP calls.

use lru::LruCache;
use serde::Deserialize;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Default HTTP timeout for GitHub API requests (3 seconds).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// Maximum cache entries.
const CACHE_CAPACITY: usize = 1024;

/// Cache TTL (5 minutes).
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Number of consecutive failures before circuit opens.
const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;

/// How long the circuit stays open (60 seconds).
const CIRCUIT_BREAKER_COOLDOWN: Duration = Duration::from_secs(60);

/// Cache entry with expiration.
#[derive(Clone, Debug)]
struct CacheEntry<T> {
    value: T,
    inserted_at: Instant,
}

impl<T> CacheEntry<T> {
    fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() > CACHE_TTL
    }
}

/// Circuit breaker state.
#[derive(Debug)]
struct CircuitBreaker {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
}

impl CircuitBreaker {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            opened_at: None,
        }
    }

    /// Returns true if requests should be allowed through.
    fn allow_request(&self) -> bool {
        if let Some(opened_at) = self.opened_at {
            // Allow a probe request after cooldown
            opened_at.elapsed() > CIRCUIT_BREAKER_COOLDOWN
        } else {
            true
        }
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.opened_at = None;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= CIRCUIT_BREAKER_THRESHOLD {
            if self.opened_at.is_none() {
                warn!(
                    threshold = CIRCUIT_BREAKER_THRESHOLD,
                    cooldown_secs = CIRCUIT_BREAKER_COOLDOWN.as_secs(),
                    "GitHub API circuit breaker opened"
                );
            }
            self.opened_at = Some(Instant::now());
        }
    }
}

/// GitHub API client with built-in caching and circuit breaker.
pub struct GitHubClient {
    http: reqwest::Client,
    token: Option<String>,
    /// Cache for repo metadata (keyed by "owner/repo").
    repo_cache: Mutex<LruCache<String, CacheEntry<RepoInfo>>>,
    /// Cache for issue/PR metadata (keyed by "owner/repo/number").
    issue_cache: Mutex<LruCache<String, CacheEntry<IssueInfo>>>,
    /// Cache for repo languages (keyed by "owner/repo").
    #[allow(clippy::type_complexity)]
    languages_cache: Mutex<LruCache<String, CacheEntry<Vec<(String, u64)>>>>,
    /// Circuit breaker.
    circuit: Mutex<CircuitBreaker>,
}

/// Repository metadata from the GitHub API.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoInfo {
    pub full_name: String,
    pub description: Option<String>,
    pub stargazers_count: u64,
    pub forks_count: u64,
    pub default_branch: String,
    pub topics: Vec<String>,
    pub license: Option<LicenseInfo>,
    pub owner: OwnerInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OwnerInfo {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LicenseInfo {
    pub spdx_id: Option<String>,
    pub name: String,
}

/// Issue or PR metadata from the GitHub API.
///
/// GitHub's Issues API returns both issues and PRs. PRs have `pull_request != null`.
#[derive(Debug, Clone, Deserialize)]
pub struct IssueInfo {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub user: UserInfo,
    pub assignee: Option<UserInfo>,
    pub labels: Vec<LabelInfo>,
    /// Present when this is actually a PR.
    pub pull_request: Option<PullRequestRef>,
    /// Draft status (only on PR-detail endpoint, not issues endpoint).
    pub draft: Option<bool>,
    /// Merged status (only on PR-detail endpoint).
    pub merged: Option<bool>,
    pub base: Option<BranchRef>,
    pub head: Option<BranchRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LabelInfo {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestRef {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BranchRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
}

impl GitHubClient {
    /// Create a new client.
    ///
    /// If `token` is `Some`, it will be used for `Authorization: Bearer` headers.
    pub fn new(token: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .user_agent("waddle-xmpp/0.1")
            .build()
            .expect("Failed to build HTTP client");

        let cache_size = NonZeroUsize::new(CACHE_CAPACITY).unwrap();

        Self {
            http,
            token,
            repo_cache: Mutex::new(LruCache::new(cache_size)),
            issue_cache: Mutex::new(LruCache::new(cache_size)),
            languages_cache: Mutex::new(LruCache::new(cache_size)),
            circuit: Mutex::new(CircuitBreaker::new()),
        }
    }

    /// Create from environment: reads `GITHUB_TOKEN` env var.
    pub fn from_env() -> Self {
        let token = std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty());
        if token.is_some() {
            debug!("GitHub client initialized with token authentication");
        } else {
            debug!("GitHub client initialized without token (60 req/hr limit)");
        }
        Self::new(token)
    }

    /// Check circuit breaker; returns false if requests should be suppressed.
    fn check_circuit(&self) -> bool {
        let circuit = self.circuit.lock().unwrap();
        circuit.allow_request()
    }

    fn record_success(&self) {
        let mut circuit = self.circuit.lock().unwrap();
        circuit.record_success();
    }

    fn record_failure(&self) {
        let mut circuit = self.circuit.lock().unwrap();
        circuit.record_failure();
    }

    /// Build a request with optional auth header.
    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.get(url);
        if let Some(ref token) = self.token {
            req = req.bearer_auth(token);
        }
        // GitHub API v3
        req = req.header("Accept", "application/vnd.github.v3+json");
        req
    }

    /// Fetch repository metadata. Returns `None` on any error (fail-open).
    pub async fn fetch_repo(&self, owner: &str, repo: &str) -> Option<RepoInfo> {
        let key = format!("{owner}/{repo}");

        // Check cache
        {
            let mut cache = self.repo_cache.lock().unwrap();
            if let Some(entry) = cache.get(&key) {
                if !entry.is_expired() {
                    debug!(repo = %key, "GitHub repo cache hit");
                    return Some(entry.value.clone());
                }
                // Expired — remove and re-fetch
                cache.pop(&key);
            }
        }

        if !self.check_circuit() {
            debug!("GitHub circuit breaker open, skipping repo fetch");
            return None;
        }

        let url = format!("https://api.github.com/repos/{owner}/{repo}");
        match self.request(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.record_success();
                match resp.json::<RepoInfo>().await {
                    Ok(info) => {
                        let mut cache = self.repo_cache.lock().unwrap();
                        cache.put(
                            key,
                            CacheEntry {
                                value: info.clone(),
                                inserted_at: Instant::now(),
                            },
                        );
                        Some(info)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to parse GitHub repo response");
                        None
                    }
                }
            }
            Ok(resp) => {
                let status = resp.status();
                warn!(status = %status, repo = %key, "GitHub API returned error for repo");
                self.record_failure();
                None
            }
            Err(e) => {
                warn!(error = %e, repo = %key, "GitHub API request failed for repo");
                self.record_failure();
                None
            }
        }
    }

    /// Fetch repository languages. Returns empty vec on error.
    pub async fn fetch_languages(&self, owner: &str, repo: &str) -> Vec<(String, u64)> {
        let key = format!("{owner}/{repo}");

        // Check cache
        {
            let mut cache = self.languages_cache.lock().unwrap();
            if let Some(entry) = cache.get(&key) {
                if !entry.is_expired() {
                    debug!(repo = %key, "GitHub languages cache hit");
                    return entry.value.clone();
                }
                cache.pop(&key);
            }
        }

        if !self.check_circuit() {
            return Vec::new();
        }

        let url = format!("https://api.github.com/repos/{owner}/{repo}/languages");
        match self.request(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.record_success();
                match resp.json::<HashMap<String, u64>>().await {
                    Ok(map) => {
                        let mut langs: Vec<(String, u64)> = map.into_iter().collect();
                        // Sort by bytes descending
                        langs.sort_by(|a, b| b.1.cmp(&a.1));

                        let mut cache = self.languages_cache.lock().unwrap();
                        cache.put(
                            key,
                            CacheEntry {
                                value: langs.clone(),
                                inserted_at: Instant::now(),
                            },
                        );
                        langs
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to parse GitHub languages response");
                        Vec::new()
                    }
                }
            }
            Ok(resp) => {
                warn!(status = %resp.status(), "GitHub API error for languages");
                self.record_failure();
                Vec::new()
            }
            Err(e) => {
                warn!(error = %e, "GitHub API request failed for languages");
                self.record_failure();
                Vec::new()
            }
        }
    }

    /// Fetch issue metadata. Returns `None` on error.
    pub async fn fetch_issue(&self, owner: &str, repo: &str, number: u64) -> Option<IssueInfo> {
        let key = format!("{owner}/{repo}/{number}");

        {
            let mut cache = self.issue_cache.lock().unwrap();
            if let Some(entry) = cache.get(&key) {
                if !entry.is_expired() {
                    debug!(issue = %key, "GitHub issue cache hit");
                    return Some(entry.value.clone());
                }
                cache.pop(&key);
            }
        }

        if !self.check_circuit() {
            return None;
        }

        let url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}");
        match self.request(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.record_success();
                match resp.json::<IssueInfo>().await {
                    Ok(info) => {
                        let mut cache = self.issue_cache.lock().unwrap();
                        cache.put(
                            key,
                            CacheEntry {
                                value: info.clone(),
                                inserted_at: Instant::now(),
                            },
                        );
                        Some(info)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to parse GitHub issue response");
                        None
                    }
                }
            }
            Ok(resp) => {
                warn!(status = %resp.status(), issue = %key, "GitHub API error for issue");
                self.record_failure();
                None
            }
            Err(e) => {
                warn!(error = %e, issue = %key, "GitHub API request failed for issue");
                self.record_failure();
                None
            }
        }
    }

    /// Fetch pull request metadata. Uses the PR-specific endpoint for draft/merged fields.
    pub async fn fetch_pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Option<IssueInfo> {
        let key = format!("{owner}/{repo}/pr/{number}");

        {
            let mut cache = self.issue_cache.lock().unwrap();
            if let Some(entry) = cache.get(&key) {
                if !entry.is_expired() {
                    debug!(pr = %key, "GitHub PR cache hit");
                    return Some(entry.value.clone());
                }
                cache.pop(&key);
            }
        }

        if !self.check_circuit() {
            return None;
        }

        let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{number}");
        match self.request(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.record_success();
                match resp.json::<IssueInfo>().await {
                    Ok(info) => {
                        let mut cache = self.issue_cache.lock().unwrap();
                        cache.put(
                            key,
                            CacheEntry {
                                value: info.clone(),
                                inserted_at: Instant::now(),
                            },
                        );
                        Some(info)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to parse GitHub PR response");
                        None
                    }
                }
            }
            Ok(resp) => {
                warn!(status = %resp.status(), "GitHub API error for PR");
                self.record_failure();
                None
            }
            Err(e) => {
                warn!(error = %e, "GitHub API request failed for PR");
                self.record_failure();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_allows_initially() {
        let cb = CircuitBreaker::new();
        assert!(cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            cb.record_failure();
        }
        assert!(!cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD - 1 {
            cb.record_failure();
        }
        cb.record_success();
        assert_eq!(cb.consecutive_failures, 0);
        assert!(cb.allow_request());
    }

    #[test]
    fn test_client_from_env_no_token() {
        // Just ensure it doesn't panic
        std::env::remove_var("GITHUB_TOKEN");
        let client = GitHubClient::from_env();
        assert!(client.token.is_none());
    }
}
