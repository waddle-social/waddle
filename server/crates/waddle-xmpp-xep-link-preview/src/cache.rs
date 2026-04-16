//! URL-keyed LRU cache for parsed previews.
//!
//! Two separate caches share a URL key space:
//! - *positive* — successful fetches returning a [`LinkPreview`], kept
//!   for [`CacheConfig::positive_ttl`].
//! - *negative* — URLs that recently failed to produce a preview, kept
//!   for the shorter [`CacheConfig::negative_ttl`] so a transiently
//!   broken upstream doesn't stay un-previewed for a full hour.

use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;

use crate::LinkPreview;

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub capacity: u64,
    pub positive_ttl: Duration,
    pub negative_ttl: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            capacity: 10_000,
            positive_ttl: Duration::from_secs(60 * 60),
            negative_ttl: Duration::from_secs(5 * 60),
        }
    }
}

/// Cached lookup outcome — either a fresh preview, an explicit negative
/// marker, or nothing remembered.
#[derive(Debug, Clone)]
pub enum Lookup {
    Hit(Arc<LinkPreview>),
    Negative,
    Miss,
}

pub struct PreviewCache {
    positive: Cache<String, Arc<LinkPreview>>,
    negative: Cache<String, ()>,
}

impl PreviewCache {
    pub fn new(config: &CacheConfig) -> Self {
        Self {
            positive: Cache::builder()
                .max_capacity(config.capacity)
                .time_to_live(config.positive_ttl)
                .build(),
            negative: Cache::builder()
                .max_capacity(config.capacity)
                .time_to_live(config.negative_ttl)
                .build(),
        }
    }

    pub async fn lookup(&self, url: &str) -> Lookup {
        if let Some(hit) = self.positive.get(url).await {
            return Lookup::Hit(hit);
        }
        if self.negative.get(url).await.is_some() {
            return Lookup::Negative;
        }
        Lookup::Miss
    }

    pub async fn insert_positive(&self, url: String, preview: LinkPreview) {
        self.positive.insert(url, Arc::new(preview)).await;
    }

    pub async fn insert_negative(&self, url: String) {
        self.negative.insert(url, ()).await;
    }

    /// Sync the caches — ensures all pending tasks complete before a
    /// subsequent lookup, primarily useful in tests.
    pub async fn sync(&self) {
        self.positive.run_pending_tasks().await;
        self.negative.run_pending_tasks().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LinkPreview;

    fn test_config(positive: Duration, negative: Duration) -> CacheConfig {
        CacheConfig {
            capacity: 16,
            positive_ttl: positive,
            negative_ttl: negative,
        }
    }

    fn sample() -> LinkPreview {
        LinkPreview {
            url: "https://example.com/a".to_owned(),
            canonical_url: Some("https://example.com/a".to_owned()),
            title: Some("T".to_owned()),
            description: None,
            site_name: None,
            type_: None,
            image: None,
        }
    }

    #[tokio::test]
    async fn miss_on_empty_cache() {
        let cache = PreviewCache::new(&CacheConfig::default());
        assert!(matches!(cache.lookup("https://example.com/a").await, Lookup::Miss));
    }

    #[tokio::test]
    async fn hit_after_insert_positive() {
        let cache = PreviewCache::new(&CacheConfig::default());
        cache
            .insert_positive("https://example.com/a".to_owned(), sample())
            .await;
        cache.sync().await;
        let lookup = cache.lookup("https://example.com/a").await;
        match lookup {
            Lookup::Hit(p) => assert_eq!(p.title.as_deref(), Some("T")),
            other => panic!("expected hit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn negative_marker_survives_separately() {
        let cache = PreviewCache::new(&CacheConfig::default());
        cache.insert_negative("https://broken.example/".to_owned()).await;
        cache.sync().await;
        assert!(matches!(
            cache.lookup("https://broken.example/").await,
            Lookup::Negative
        ));
        assert!(matches!(
            cache.lookup("https://other.example/").await,
            Lookup::Miss
        ));
    }

    #[tokio::test]
    async fn positive_entries_expire_after_ttl() {
        let cache = PreviewCache::new(&test_config(
            Duration::from_millis(20),
            Duration::from_millis(20),
        ));
        cache
            .insert_positive("https://x.example/".to_owned(), sample())
            .await;
        cache.sync().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        cache.sync().await;
        assert!(matches!(cache.lookup("https://x.example/").await, Lookup::Miss));
    }

    #[tokio::test]
    async fn negative_ttl_shorter_than_positive_by_default() {
        let config = CacheConfig::default();
        assert!(config.negative_ttl < config.positive_ttl);
    }
}
