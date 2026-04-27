use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use oci_client::client::{Client, ClientConfig, ClientProtocol, ImageLayer};
use oci_client::manifest::OciDescriptor;
use oci_client::secrets::RegistryAuth;
use oci_client::Reference;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

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
const MAX_WASM_BYTES: usize = 50 * 1024 * 1024;
const CORE_WASM_VERSION: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
const COMPONENT_WASM_VERSION: [u8; 4] = [0x0d, 0x00, 0x01, 0x00];

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

        let cached = self.cached_wasm_path(module)?;
        if cached.exists() {
            match validate_cached_wasm_file(&module.name, &cached) {
                Ok(()) => return Ok(cached),
                Err(error) => {
                    warn!(
                        extension = %module.name,
                        cache_path = %cached.display(),
                        %error,
                        "cached extension wasm failed validation; re-pulling artifact"
                    );
                    std::fs::remove_file(&cached).with_context(|| {
                        format!(
                            "failed to remove invalid cached extension {}",
                            cached.display()
                        )
                    })?;
                    let _ = std::fs::remove_file(cached_wasm_digest_path(&cached));
                }
            }
        }

        self.pull_module(module)
            .await
            .with_context(|| format!("failed to pull extension {}", module.name))
    }

    fn cached_wasm_path(&self, module: &ExtensionModuleConfig) -> Result<PathBuf> {
        let module_name = validate_cache_component("name", &module.name)?;
        let (_registry, digest) = module
            .oci_registry_and_digest()
            .map_err(anyhow::Error::msg)?;
        let digest = cache_digest_component(digest)?;
        Ok(self
            .cache_dir
            .join(module_name)
            .join(format!("{digest}.wasm")))
    }

    pub async fn pull_module(&self, module: &ExtensionModuleConfig) -> Result<PathBuf> {
        let reference = self.reference_for(module)?;
        let cached = self.cached_wasm_path(module)?;
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

        let artifact_type = manifest.artifact_type.as_deref().ok_or_else(|| {
            anyhow!(
                "missing artifact type for {}: expected {}",
                module.name,
                EXTENSION_ARTIFACT_MEDIA_TYPE
            )
        })?;
        if artifact_type != EXTENSION_ARTIFACT_MEDIA_TYPE {
            bail!(
                "unexpected artifact type for {}: got {}, expected {}",
                module.name,
                artifact_type,
                EXTENSION_ARTIFACT_MEDIA_TYPE
            );
        }

        let layer = select_wasm_layer(&module.name, &image.layers, &manifest.layers)?;
        validate_wasm_layer(&module.name, layer)?;
        write_wasm_atomically(&cached, &layer.data)
            .with_context(|| format!("failed to write cached extension {}", cached.display()))?;
        write_cached_wasm_digest(&cached, layer.data.as_ref())?;
        validate_cached_wasm_file(&module.name, &cached)?;

        info!(
            extension = %module.name,
            registry = %reference.registry(),
            digest = %reference.digest().unwrap_or("<missing>"),
            cache_path = %cached.display(),
            "pulled extension OCI artifact"
        );
        Ok(cached)
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    fn reference_for(&self, module: &ExtensionModuleConfig) -> Result<Reference> {
        let (registry, digest) = module
            .oci_registry_and_digest()
            .map_err(anyhow::Error::msg)?;
        let target = format!("{registry}@{digest}");
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
    validate_wasm_bytes(module_name, layer.data.as_ref())
}

fn validate_wasm_bytes(module_name: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() < 8 {
        bail!("extension {module_name} wasm payload is too small");
    }
    if bytes.len() > MAX_WASM_BYTES {
        bail!("extension {module_name} wasm payload exceeds max size of {MAX_WASM_BYTES} bytes");
    }
    if bytes[..4] != [0x00, 0x61, 0x73, 0x6d] {
        bail!("extension {module_name} payload is not a wasm binary");
    }
    let version = [bytes[4], bytes[5], bytes[6], bytes[7]];
    if version != CORE_WASM_VERSION && version != COMPONENT_WASM_VERSION {
        bail!("extension {module_name} uses unsupported wasm binary version");
    }
    Ok(())
}

fn validate_cached_wasm_file(module_name: &str, path: &Path) -> Result<()> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read cached extension {}", path.display()))?;
    validate_wasm_bytes(module_name, &bytes)?;
    validate_cached_wasm_digest(module_name, path, &bytes)
}

fn validate_cached_wasm_digest(module_name: &str, path: &Path, bytes: &[u8]) -> Result<()> {
    let digest_path = cached_wasm_digest_path(path);
    let expected = std::fs::read_to_string(&digest_path).with_context(|| {
        format!(
            "failed to read cached extension digest {}",
            digest_path.display()
        )
    })?;
    let actual = format!("sha256:{:x}", Sha256::digest(bytes));
    if actual != expected.trim() {
        bail!("extension {module_name} wasm digest mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

fn write_cached_wasm_digest(path: &Path, bytes: &[u8]) -> Result<()> {
    let digest_path = cached_wasm_digest_path(path);
    let digest = format!("sha256:{:x}\n", Sha256::digest(bytes));
    std::fs::write(&digest_path, digest).with_context(|| {
        format!(
            "failed to write cached extension digest {}",
            digest_path.display()
        )
    })
}

fn cached_wasm_digest_path(path: &Path) -> PathBuf {
    path.with_extension("wasm.sha256")
}

fn validate_cache_component<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    if value.is_empty() {
        bail!("extension {field} must not be empty");
    }
    if value == "." || value == ".." {
        bail!("extension {field} must not be '.' or '..'");
    }
    if value.contains('/') || value.contains('\\') {
        bail!("extension {field} must not include path separators");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
    {
        bail!("extension {field} contains unsupported characters");
    }
    Ok(value)
}

fn cache_digest_component(digest: &str) -> Result<String> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        bail!("extension digest must use sha256:<64 hex>");
    };
    if hex.len() != 64 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("extension digest must use sha256:<64 hex>");
    }
    Ok(format!("sha256-{hex}"))
}

fn write_wasm_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "cached extension path has no parent directory: {}",
            path.display()
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        anyhow!(
            "cached extension path has no terminal filename: {}",
            path.display()
        )
    })?;
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp_path = parent.join(format!(
        ".{}.tmp-{}-{suffix}",
        file_name.to_string_lossy(),
        std::process::id()
    ));

    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .with_context(|| {
            format!(
                "failed to create temporary cache file {}",
                temp_path.display()
            )
        })?;
    file.write_all(bytes).with_context(|| {
        format!(
            "failed to write temporary cache file {}",
            temp_path.display()
        )
    })?;
    file.sync_all().with_context(|| {
        format!(
            "failed to sync temporary cache file {}",
            temp_path.display()
        )
    })?;

    match std::fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(_error) if path.exists() => {
            let _ = std::fs::remove_file(&temp_path);
            info!(
                cache_path = %path.display(),
                "cached extension was created concurrently; using existing file"
            );
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error).with_context(|| {
                format!("failed to atomically move cache file to {}", path.display())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use oci_client::manifest::OciDescriptor;
    use oci_client::Reference;

    use super::*;

    #[test]
    fn validates_reference_format() {
        let cache_dir = std::env::temp_dir().join("waddle-test");
        let puller = OciExtensionPuller::new(cache_dir);
        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: serde_json::Value::Object(Default::default()),
            config_secret_files: Default::default(),
            local_path: None,
        };

        let reference = puller
            .reference_for(&module)
            .expect("reference should parse");
        let expected: Reference =
            "ghcr.io/waddle-social/waddle/extensions/example-extension@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()
                .expect("reference should parse");
        assert_eq!(reference.registry(), expected.registry());
        assert_eq!(reference.repository(), expected.repository());
        assert_eq!(reference.digest(), expected.digest());
    }

    #[test]
    fn rejects_reference_with_mutable_registry_tag() {
        let cache_dir = std::env::temp_dir().join("waddle-test");
        let puller = OciExtensionPuller::new(cache_dir);
        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension:latest"
                .to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: serde_json::Value::Object(Default::default()),
            config_secret_files: Default::default(),
            local_path: None,
        };

        let error = puller
            .reference_for(&module)
            .expect_err("mutable registry tags should be rejected");
        assert!(error.to_string().contains("must not include a mutable tag"));
    }

    #[test]
    fn rejects_oci_module_tag_field_even_when_digest_is_set() {
        let cache_dir = std::env::temp_dir().join("waddle-test");
        let puller = OciExtensionPuller::new(cache_dir);
        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: Some("latest".to_string()),
            namespace: "urn:example:extension:1".to_string(),
            config: serde_json::Value::Object(Default::default()),
            config_secret_files: Default::default(),
            local_path: None,
        };

        let error = puller
            .reference_for(&module)
            .expect_err("tag field should be rejected for OCI modules");
        assert!(error
            .to_string()
            .contains("must use an immutable digest instead of tag"));
    }

    #[test]
    fn selects_single_wasm_layer() {
        let layers = vec![
            ImageLayer {
                data: vec![0u8; 8],
                media_type: "application/octet-stream".to_string(),
                annotations: None,
            },
            ImageLayer {
                data: vec![0, 97, 115, 109, 1, 0, 0, 0],
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
            select_wasm_layer("example-extension", &layers, &descriptors).expect("wasm selected");
        assert_eq!(layer.data[..4], [0, 97, 115, 109]);
    }

    #[test]
    fn rejects_invalid_wasm_payload() {
        let layer = ImageLayer {
            data: vec![1, 2, 3, 4, 0, 0, 0, 0],
            media_type: "application/wasm".to_string(),
            annotations: None,
        };
        let error = validate_wasm_layer("example-extension", &layer)
            .expect_err("invalid payload should fail");
        assert!(error.to_string().contains("payload is not a wasm binary"));
    }

    #[test]
    fn accepts_wasm_component_payload() {
        let layer = ImageLayer {
            data: vec![0, 97, 115, 109, 0x0d, 0, 1, 0],
            media_type: "application/wasm".to_string(),
            annotations: None,
        };

        validate_wasm_layer("example-extension", &layer)
            .expect("component payload should validate");
    }

    #[test]
    fn rejects_unknown_wasm_binary_version() {
        let layer = ImageLayer {
            data: vec![0, 97, 115, 109, 2, 0, 0, 0],
            media_type: "application/wasm".to_string(),
            annotations: None,
        };

        let error = validate_wasm_layer("example-extension", &layer)
            .expect_err("unknown binary version should fail");
        assert!(error
            .to_string()
            .contains("uses unsupported wasm binary version"));
    }

    #[test]
    fn rejects_invalid_cache_component() {
        let puller = OciExtensionPuller::new(std::env::temp_dir().join("waddle-test"));
        let module = ExtensionModuleConfig {
            name: "../bad".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: serde_json::Value::Object(Default::default()),
            config_secret_files: Default::default(),
            local_path: None,
        };

        let error = puller
            .cached_wasm_path(&module)
            .expect_err("cache path should reject path traversal");
        assert!(error
            .to_string()
            .contains("must not include path separators"));
    }

    #[test]
    fn cache_path_uses_sanitized_digest() {
        let puller = OciExtensionPuller::new(std::env::temp_dir().join("waddle-test"));
        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: serde_json::Value::Object(Default::default()),
            config_secret_files: Default::default(),
            local_path: None,
        };

        let path = puller
            .cached_wasm_path(&module)
            .expect("cache path should be valid");
        assert!(path.ends_with(
            "example-extension/sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.wasm"
        ));
    }

    #[test]
    fn cached_wasm_requires_matching_sidecar_digest() {
        let root = std::env::temp_dir().join(format!(
            "waddle-extension-cache-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("cache test dir");
        let wasm_path = root.join("example-extension.wasm");
        let wasm = [0, 97, 115, 109, 0x0d, 0, 1, 0];
        std::fs::write(&wasm_path, wasm).expect("wasm fixture");

        let missing = validate_cached_wasm_file("example-extension", &wasm_path)
            .expect_err("cache without digest sidecar should be rejected");
        assert!(missing
            .to_string()
            .contains("failed to read cached extension digest"));

        std::fs::write(
            cached_wasm_digest_path(&wasm_path),
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        )
        .expect("digest sidecar");
        let mismatch = validate_cached_wasm_file("example-extension", &wasm_path)
            .expect_err("cache with wrong digest sidecar should be rejected");
        assert!(mismatch.to_string().contains("wasm digest mismatch"));

        write_cached_wasm_digest(&wasm_path, &wasm).expect("valid digest sidecar");
        validate_cached_wasm_file("example-extension", &wasm_path)
            .expect("cache with matching digest sidecar should validate");

        let _ = std::fs::remove_dir_all(root);
    }
}
