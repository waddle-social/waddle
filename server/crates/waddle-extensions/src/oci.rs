use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use oci_client::client::{Client, ClientConfig, ClientProtocol, ImageLayer};
use oci_client::manifest::OciDescriptor;
use oci_client::secrets::RegistryAuth;
use oci_client::Reference;
use tracing::info;

use crate::config::ExtensionModuleConfig;

/// Resolves an extension module configuration to a WASM component file on disk.
///
/// Uses `local_path` when provided (dev/test mode). Otherwise it resolves the
/// configured OCI reference and writes the module payload into `cache_dir`.
#[derive(Debug, Clone)]
pub struct OciExtensionPuller {
    pub cache_dir: PathBuf,
}

const WASM_LAYER_MEDIA_TYPE: &str = "application/wasm";
const EXTENSION_ARTIFACT_MEDIA_TYPE: &str = "application/vnd.waddle.extension.wasm.v1+wasm";

impl OciExtensionPuller {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
        }
    }

    /// Resolve a module config to a concrete WASM file path on disk.
    pub async fn resolve_wasm_path(&self, module: &ExtensionModuleConfig) -> Result<PathBuf> {
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

        self.pull_module(module)
            .await
            .with_context(|| format!("failed to pull extension {}", module.name))
    }

    fn cached_wasm_path(&self, module: &ExtensionModuleConfig) -> PathBuf {
        self.cache_dir
            .join(&module.name)
            .join(format!("{}.wasm", module.tag))
    }

    pub async fn pull_module(&self, module: &ExtensionModuleConfig) -> Result<PathBuf> {
        let reference = self.reference_for(module)?;
        let cached = self.cached_wasm_path(module);
        if let Some(parent) = cached.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create extension cache dir {}", parent.display())
            })?;
        }

        let client = Client::new(ClientConfig {
            protocol: ClientProtocol::Https,
            ..Default::default()
        });

        let image = client
            .pull(
                &reference,
                &RegistryAuth::Anonymous,
                vec![WASM_LAYER_MEDIA_TYPE],
            )
            .await
            .with_context(|| format!("failed to pull OCI artifact {reference}"))?;

        let manifest = image
            .manifest
            .ok_or_else(|| anyhow!("OCI artifact {reference} returned no manifest"))?;

        if let Some(artifact_type) = manifest.artifact_type.as_deref() {
            if artifact_type != EXTENSION_ARTIFACT_MEDIA_TYPE {
                bail!(
                    "unexpected artifact type for {}: got {}, expected {}",
                    module.name,
                    artifact_type,
                    EXTENSION_ARTIFACT_MEDIA_TYPE
                );
            }
        }

        let layer = select_wasm_layer(&module.name, &image.layers, &manifest.layers)?;
        validate_wasm_layer(&module.name, layer)?;
        std::fs::write(&cached, &layer.data)
            .with_context(|| format!("failed to write cached extension {}", cached.display()))?;

        info!(
            extension = %module.name,
            registry = %module.registry,
            tag = %module.tag,
            cache_path = %cached.display(),
            "pulled extension OCI artifact"
        );
        Ok(cached)
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    fn reference_for(&self, module: &ExtensionModuleConfig) -> Result<Reference> {
        let target = format!("{}:{}", module.registry, module.tag);
        target
            .parse()
            .with_context(|| format!("invalid OCI reference {target}"))
    }
}

fn select_wasm_layer<'a>(
    module_name: &str,
    layers: &'a [ImageLayer],
    descriptors: &'a [OciDescriptor],
) -> Result<&'a ImageLayer> {
    let mut selected: Option<&ImageLayer> = None;
    for (index, layer) in layers.iter().enumerate() {
        let media_type = descriptors
            .get(index)
            .map(|descriptor| descriptor.media_type.as_str())
            .unwrap_or(layer.media_type.as_str());
        if media_type != WASM_LAYER_MEDIA_TYPE {
            continue;
        }
        if selected.is_some() {
            bail!("extension {module_name} OCI artifact has multiple wasm layers");
        }
        selected = Some(layer);
    }

    selected.ok_or_else(|| anyhow!("extension {module_name} OCI artifact has no wasm layer"))
}

fn validate_wasm_layer(module_name: &str, layer: &ImageLayer) -> Result<()> {
    if layer.data.len() < 4 {
        bail!("extension {module_name} wasm payload is too small");
    }
    if layer.data[..4] != [0x00, 0x61, 0x73, 0x6d] {
        bail!("extension {module_name} payload is not a wasm binary");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use oci_client::manifest::OciDescriptor;
    use oci_client::Reference;

    use super::*;

    #[test]
    fn validates_reference_format() {
        let puller = OciExtensionPuller::new("/tmp/waddle-test");
        let module = ExtensionModuleConfig {
            name: "github-enricher".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/github-enricher".to_string(),
            tag: "sha-abc123".to_string(),
            namespace: "urn:waddle:github:0".to_string(),
            config: serde_json::Value::Object(Default::default()),
            config_secret_files: Default::default(),
            local_path: None,
        };

        let reference = puller
            .reference_for(&module)
            .expect("reference should parse");
        let expected: Reference =
            "ghcr.io/waddle-social/waddle/extensions/github-enricher:sha-abc123"
                .parse()
                .expect("reference should parse");
        assert_eq!(reference.registry(), expected.registry());
        assert_eq!(reference.repository(), expected.repository());
        assert_eq!(reference.tag(), expected.tag());
    }

    #[test]
    fn selects_single_wasm_layer() {
        let layers = vec![
            ImageLayer {
                data: vec![0u8; 8].into(),
                media_type: "application/octet-stream".to_string(),
                annotations: None,
            },
            ImageLayer {
                data: vec![0, 97, 115, 109, 1, 0, 0, 0].into(),
                media_type: "application/wasm".to_string(),
                annotations: None,
            },
        ];
        let descriptors = vec![
            OciDescriptor {
                media_type: "application/octet-stream".to_string(),
                digest: String::new(),
                size: 8,
                urls: None,
                annotations: None,
            },
            OciDescriptor {
                media_type: "application/wasm".to_string(),
                digest: String::new(),
                size: 8,
                urls: None,
                annotations: None,
            },
        ];

        let layer =
            select_wasm_layer("github-enricher", &layers, &descriptors).expect("wasm selected");
        assert_eq!(layer.data[..4], [0, 97, 115, 109]);
    }

    #[test]
    fn rejects_invalid_wasm_payload() {
        let layer = ImageLayer {
            data: vec![1, 2, 3, 4, 5].into(),
            media_type: "application/wasm".to_string(),
            annotations: None,
        };
        let error = validate_wasm_layer("github-enricher", &layer)
            .expect_err("invalid payload should fail");
        assert!(error.to_string().contains("payload is not a wasm binary"));
    }
}
