use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::config::ExtensionModuleConfig;

/// Resolves an extension module configuration to a WASM component file on disk.
///
/// Uses `local_path` when provided (dev/test mode). OCI pulling from a remote
/// registry is not yet implemented; returns an error when neither a `local_path`
/// nor a cached artifact under `cache_dir` is available.
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

    /// Resolve a module config to a concrete WASM file path on disk.
    pub fn resolve_wasm_path(&self, module: &ExtensionModuleConfig) -> Result<PathBuf> {
        if let Some(local) = module.local_path.as_ref() {
            let path = PathBuf::from(local);
            if !path.exists() {
                bail!(
                    "extension {} local_path does not exist: {}",
                    module.name,
                    path.display()
                );
            }
            return Ok(path);
        }

        let cached = self.cached_wasm_path(module);
        if cached.exists() {
            return Ok(cached);
        }

        bail!(
            "extension {}: no local_path set and no cached artifact at {} (OCI pulling not yet implemented)",
            module.name,
            cached.display()
        )
    }

    fn cached_wasm_path(&self, module: &ExtensionModuleConfig) -> PathBuf {
        self.cache_dir
            .join(&module.name)
            .join(format!("{}.wasm", module.tag))
    }

    /// Stub for future OCI artifact pulling from a registry like GHCR.
    pub fn pull_module(&self, _module: &ExtensionModuleConfig) -> Result<Option<PathBuf>> {
        // TODO: Implement OCI artifact pulling (oci-distribution or oras).
        Ok(None)
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}
