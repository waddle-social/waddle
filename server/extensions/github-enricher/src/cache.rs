use lru::LruCache;
use std::num::NonZeroUsize;

pub struct MetadataCache {
    cache: LruCache<String, String>,
}

impl MetadataCache {
    pub fn new() -> Self {
        Self {
            cache: LruCache::new(NonZeroUsize::new(256).expect("non-zero")),
        }
    }

    pub fn get(&mut self, key: &str) -> Option<String> {
        self.cache.get(key).cloned()
    }

    pub fn put(&mut self, key: String, value: String) {
        self.cache.put(key, value);
    }
}
