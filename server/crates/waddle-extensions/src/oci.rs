use std::path::PathBuf;

use anyhow::Result;

use crate::config::ExtensionModuleConfig;

#[derive(Debug, Clone)]
pub struct OciExtensionPuller {
    pub cache_dir: PathBuf,
}

impl OciExtensionPuller {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
        }
    }

    pub fn pull_module(&self, module: &ExtensionModuleConfig) -> Result<Option<PathBuf>> {
        let _ = &self.cache_dir;
        let _ = module;
        // Pulling from GHCR is intentionally fail-open for now.
        Ok(None)
    }
}
