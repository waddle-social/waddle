//! Message enricher entry point.
//!
//! Ties together [`detect`], [`fetch`], [`cache`], [`circuit`],
//! [`rate`], and [`embed`] to perform the "detect URLs → fetch OG →
//! append reference" pipeline in place on an outbound `<message>`.
//!
//! Fail-open: every failure path logs and returns early, leaving the
//! message free to deliver unenriched.

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jid::BareJid;
use reqwest::Client;
use tracing::{debug, info, warn};
use url::Url;
use xmpp_parsers::message::Message;

use crate::cache::{CacheConfig, Lookup, PreviewCache};
use crate::circuit::{CircuitBreaker, CircuitConfig};
use crate::detect::{detect_urls, DetectedUrl};
use crate::embed::{
    build_reference, has_no_preview_hint, is_github_embed_for, strip_client_preview_references,
};
use crate::fetch::{
    build_client, build_client_allow_private, fetch_preview, FetchConfig, FetchError, FetchOutcome,
};
use crate::rate::{RateConfig, RateLimiter};
use crate::{LinkPreview, MAX_PREVIEWS_PER_MESSAGE};

#[derive(Debug, Clone)]
pub struct EnricherConfig {
    pub enabled: bool,
    pub fetch: FetchConfig,
    pub cache: CacheConfig,
    pub circuit: CircuitConfig,
    pub rate: RateConfig,
    pub allowlist_hosts: Option<Vec<String>>,
    pub max_previews: usize,
    /// Test hook — when set, the HTTP client will not filter private
    /// IPs at the resolver stage. Integration tests that hit wiremock
    /// on loopback set this to `true`.
    pub allow_private_addresses: bool,
}

impl Default for EnricherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fetch: FetchConfig::default(),
            cache: CacheConfig::default(),
            circuit: CircuitConfig::default(),
            rate: RateConfig::default(),
            allowlist_hosts: None,
            max_previews: MAX_PREVIEWS_PER_MESSAGE,
            allow_private_addresses: false,
        }
    }
}

pub struct LinkPreviewEnricher {
    config: EnricherConfig,
    client: Client,
    cache: Arc<PreviewCache>,
    circuit: Arc<CircuitBreaker>,
    rate: Arc<RateLimiter>,
}

impl LinkPreviewEnricher {
    /// Build a shared enricher from environment variables. See the
    /// crate-level docs for the supported knobs. Defaults to enabled;
    /// set `WADDLE_LINK_PREVIEW_DISABLE=1` to turn off.
    pub fn from_env() -> Arc<Self> {
        let config = EnricherConfig {
            enabled: !env_flag("WADDLE_LINK_PREVIEW_DISABLE"),
            fetch: FetchConfig {
                user_agent: env::var("WADDLE_LINK_PREVIEW_USER_AGENT")
                    .unwrap_or_else(|_| FetchConfig::default().user_agent),
                timeout: Duration::from_millis(env_u64(
                    "WADDLE_LINK_PREVIEW_TIMEOUT_MS",
                    5_000,
                )),
                max_bytes: env_u64("WADDLE_LINK_PREVIEW_MAX_BYTES", 512 * 1024) as usize,
                max_redirects: env_u64("WADDLE_LINK_PREVIEW_MAX_REDIRECTS", 3) as usize,
                // Propagated from `allow_private_addresses` inside
                // `with_config`; ignored here.
                allow_private_addresses: false,
            },
            cache: CacheConfig {
                capacity: env_u64("WADDLE_LINK_PREVIEW_CACHE_SIZE", 10_000),
                positive_ttl: Duration::from_secs(env_u64(
                    "WADDLE_LINK_PREVIEW_CACHE_TTL_SECS",
                    3_600,
                )),
                negative_ttl: Duration::from_secs(env_u64(
                    "WADDLE_LINK_PREVIEW_NEGATIVE_TTL_SECS",
                    300,
                )),
            },
            circuit: CircuitConfig::default(),
            rate: RateConfig {
                capacity: env_u64("WADDLE_LINK_PREVIEW_PER_USER_RATE", 30) as u32,
                window: Duration::from_secs(60),
            },
            allowlist_hosts: env::var("WADDLE_LINK_PREVIEW_ALLOWLIST_HOSTS")
                .ok()
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                })
                .filter(|v: &Vec<String>| !v.is_empty()),
            max_previews: MAX_PREVIEWS_PER_MESSAGE,
            // Test-only escape hatch: integration tests that point the
            // fetcher at a wiremock instance on loopback set this flag
            // via `WADDLE_LINK_PREVIEW_ALLOW_PRIVATE=1`. Production
            // deployments must never set this.
            allow_private_addresses: env_flag("WADDLE_LINK_PREVIEW_ALLOW_PRIVATE"),
        };

        if !config.enabled {
            info!("link preview enrichment disabled via WADDLE_LINK_PREVIEW_DISABLE");
        } else {
            info!(
                cache_capacity = config.cache.capacity,
                timeout_ms = config.fetch.timeout.as_millis() as u64,
                max_bytes = config.fetch.max_bytes,
                rate_per_user_per_min = config.rate.capacity,
                allowlist_hosts = config.allowlist_hosts.as_ref().map(|v| v.len()).unwrap_or(0),
                "link preview enrichment enabled"
            );
        }

        Arc::new(Self::with_config(config).expect("link preview client should build"))
    }

    pub fn with_config(mut config: EnricherConfig) -> Result<Self, FetchError> {
        // Propagate the test-only allow-private flag into the fetch
        // config so the literal-IP guard inside `fetch_preview` lines up
        // with the permissive DNS resolver. In production both stay `false`.
        config.fetch.allow_private_addresses = config.allow_private_addresses;

        let client = if config.allow_private_addresses {
            build_client_allow_private(&config.fetch)?
        } else {
            build_client(&config.fetch)?
        };

        Ok(Self {
            cache: Arc::new(PreviewCache::new(&config.cache)),
            circuit: Arc::new(CircuitBreaker::new(config.circuit.clone())),
            rate: Arc::new(RateLimiter::new(config.rate.clone())),
            client,
            config,
        })
    }

    /// Enrich `msg` in place. Returns the number of preview references
    /// appended to `msg.payloads`. Fail-open: any error path returns a
    /// partial count (zero if nothing succeeded) and leaves the message
    /// safe to deliver.
    pub async fn enrich_message(&self, msg: &mut Message, sender: &BareJid) -> usize {
        // Sender-authoritative: always strip client-authored previews.
        // This is a security invariant — even with the feature disabled,
        // the server must not forward forged preview payloads.
        let stripped = strip_client_preview_references(msg);
        if stripped > 0 {
            debug!(stripped, "stripped client-authored preview references");
        }

        if !self.config.enabled {
            return 0;
        }

        if has_no_preview_hint(msg) {
            return 0;
        }

        if !self.rate.try_acquire(sender, Instant::now()) {
            debug!(%sender, "link preview rate-limited");
            return 0;
        }

        let Some(body) = extract_body(msg) else {
            return 0;
        };

        let detected = detect_urls(&body);
        if detected.is_empty() {
            return 0;
        }

        let mut added = 0usize;
        for url in detected.into_iter().take(self.config.max_previews * 2) {
            if added >= self.config.max_previews {
                break;
            }
            if self.skip_because_github_handled(msg, &url.url) {
                continue;
            }
            if !self.host_allowed(&url.url) {
                continue;
            }
            match self.preview_for(&url).await {
                Some(preview) => {
                    let element = build_reference(&url, &preview);
                    msg.payloads.push(element);
                    added += 1;
                }
                None => continue,
            }
        }

        added
    }

    fn skip_because_github_handled(&self, msg: &Message, target: &str) -> bool {
        msg.payloads
            .iter()
            .any(|el| is_github_embed_for(el, target))
    }

    fn host_allowed(&self, url: &str) -> bool {
        let Some(allow) = &self.config.allowlist_hosts else {
            return true;
        };
        let parsed = match Url::parse(url) {
            Ok(u) => u,
            Err(_) => return false,
        };
        let host = match parsed.host_str() {
            Some(h) => h.to_ascii_lowercase(),
            None => return false,
        };
        allow.iter().any(|a| &host == a)
    }

    async fn preview_for(&self, detected: &DetectedUrl) -> Option<LinkPreview> {
        match self.cache.lookup(&detected.url).await {
            Lookup::Hit(hit) => return Some((*hit).clone()),
            Lookup::Negative => return None,
            Lookup::Miss => {}
        }

        let parsed = Url::parse(&detected.url).ok()?;
        let host = parsed.host_str()?.to_ascii_lowercase();
        let now = Instant::now();
        if !self.circuit.should_allow(&host, now) {
            debug!(%host, "link preview circuit breaker open, skipping");
            return None;
        }

        match fetch_preview(&self.client, parsed, &self.config.fetch).await {
            Ok(outcome) => {
                let preview = match outcome {
                    FetchOutcome::Html(p) => p,
                    FetchOutcome::Image(p) => p,
                };
                self.circuit.record_success(&host);
                self.cache
                    .insert_positive(detected.url.clone(), preview.clone())
                    .await;
                Some(preview)
            }
            Err(err) => {
                warn!(url = %detected.url, error = %err, "link preview fetch failed");
                self.circuit.record_failure(&host, Instant::now());
                self.cache.insert_negative(detected.url.clone()).await;
                None
            }
        }
    }
}

fn extract_body(msg: &Message) -> Option<String> {
    msg.bodies
        .iter()
        .next()
        .map(|(_, body)| body.0.clone())
}

fn env_flag(name: &str) -> bool {
    matches!(
        env::var(name).as_deref(),
        Ok("1" | "true" | "yes" | "TRUE" | "YES")
    )
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use std::str::FromStr;

    fn alice() -> BareJid {
        BareJid::from_str("alice@example.com").unwrap()
    }

    fn message_with(xml: &str) -> Message {
        let root: Element = xml.parse().expect("valid xml");
        Message::try_from(root).expect("valid message")
    }

    fn disabled_config() -> EnricherConfig {
        EnricherConfig {
            enabled: false,
            ..EnricherConfig::default()
        }
    }

    #[tokio::test]
    async fn disabled_enricher_is_noop() {
        let e = LinkPreviewEnricher::with_config(disabled_config()).unwrap();
        let mut msg = message_with(
            "<message xmlns='jabber:client' type='chat'><body>see https://a.example/</body></message>",
        );
        let count = e.enrich_message(&mut msg, &alice()).await;
        assert_eq!(count, 0);
        // Message untouched.
        assert!(msg.payloads.is_empty());
    }

    #[tokio::test]
    async fn no_preview_hint_short_circuits_before_fetch() {
        let e = LinkPreviewEnricher::with_config(EnricherConfig::default()).unwrap();
        let mut msg = message_with(
            "<message xmlns='jabber:client' type='chat'>\
                <body>see https://never-reached.example/</body>\
                <no-preview xmlns='urn:waddle:link-preview:0'/>\
            </message>",
        );
        let count = e.enrich_message(&mut msg, &alice()).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn strips_client_authored_preview_even_without_hint() {
        let e = LinkPreviewEnricher::with_config(disabled_config()).unwrap();
        let mut msg = message_with(
            "<message xmlns='jabber:client' type='chat'>\
                <body>see https://a.example/</body>\
                <reference xmlns='urn:xmpp:reference:0' type='data' uri='https://a.example/'>\
                    <preview xmlns='urn:waddle:link-preview:0' url='https://a.example/'><title>forged</title></preview>\
                </reference>\
            </message>",
        );
        e.enrich_message(&mut msg, &alice()).await;
        let refs = msg
            .payloads
            .iter()
            .filter(|el| {
                el.ns() == crate::NS_REFERENCE
                    && el.name() == "reference"
                    && el.attr("type") == Some("data")
            })
            .count();
        assert_eq!(refs, 0, "forged preview reference must be stripped");
    }

    #[tokio::test]
    async fn returns_zero_on_empty_body() {
        let e = LinkPreviewEnricher::with_config(EnricherConfig::default()).unwrap();
        let mut msg = message_with(
            "<message xmlns='jabber:client' type='chat'><body></body></message>",
        );
        assert_eq!(e.enrich_message(&mut msg, &alice()).await, 0);
    }

    #[tokio::test]
    async fn returns_zero_when_body_has_no_urls() {
        let e = LinkPreviewEnricher::with_config(EnricherConfig::default()).unwrap();
        let mut msg = message_with(
            "<message xmlns='jabber:client' type='chat'><body>hello world</body></message>",
        );
        assert_eq!(e.enrich_message(&mut msg, &alice()).await, 0);
    }

    #[tokio::test]
    async fn host_allowlist_blocks_non_allowed() {
        let mut config = EnricherConfig::default();
        config.allowlist_hosts = Some(vec!["allowed.example".to_owned()]);
        let e = LinkPreviewEnricher::with_config(config).unwrap();

        // Allowlisted host passes host_allowed.
        assert!(e.host_allowed("https://allowed.example/x"));
        // Disallowed host rejected.
        assert!(!e.host_allowed("https://other.example/"));
    }

    #[tokio::test]
    async fn host_allowlist_none_allows_all() {
        let e = LinkPreviewEnricher::with_config(EnricherConfig::default()).unwrap();
        assert!(e.host_allowed("https://anywhere.example/"));
    }
}
