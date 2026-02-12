use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::Utc;
use glob::Pattern;
#[cfg(feature = "native")]
use oci_distribution::Reference;
#[cfg(feature = "native")]
use oci_distribution::client::{Client, ClientConfig};
#[cfg(feature = "native")]
use oci_distribution::manifest::OciImageManifest;
#[cfg(feature = "native")]
use oci_distribution::secrets::RegistryAuth;
use semver::Version;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

const VALID_EVENT_DOMAINS: &[&str] = &["system", "xmpp", "ui", "plugin"];

#[cfg(feature = "native")]
const MEDIA_TYPE_MANIFEST: &str = "application/vnd.waddle.plugin.manifest.v1+toml";
#[cfg(feature = "native")]
const MEDIA_TYPE_WASM: &str = "application/vnd.waddle.plugin.wasm.v1+wasm";
#[cfg(feature = "native")]
const MEDIA_TYPE_VUE: &str = "application/vnd.waddle.plugin.vue.v1+tar";
#[cfg(feature = "native")]
const MEDIA_TYPE_ASSETS: &str = "application/vnd.waddle.plugin.assets.v1+tar";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConfig {
    pub default_registry: String,
    pub check_updates_on_startup: bool,
    pub signature_policy: String,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            default_registry: "ghcr.io/waddle-social".to_string(),
            check_updates_on_startup: true,
            signature_policy: "warn".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginManifest {
    pub plugin: PluginMetadata,
    pub permissions: PluginPermissions,
    pub hooks: PluginHooks,
    #[serde(default)]
    pub gui: Option<PluginGui>,
    #[serde(default)]
    pub assets: Option<PluginAssets>,
}

impl PluginManifest {
    pub fn from_toml_str(manifest_toml: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(manifest_toml)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let manifest_toml =
            std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
                path: path.to_path_buf(),
                source,
            })?;

        Self::from_toml_str(&manifest_toml)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_plugin_metadata(&self.plugin)?;
        validate_permissions(&self.permissions)?;
        validate_hooks(&self.hooks, &self.permissions)?;
        validate_gui(&self.gui, &self.hooks)?;
        validate_assets(&self.assets)?;
        Ok(())
    }

    pub fn id(&self) -> &str {
        &self.plugin.id
    }

    pub fn name(&self) -> &str {
        &self.plugin.name
    }

    pub fn version(&self) -> &str {
        &self.plugin.version
    }

    pub fn capabilities(&self) -> Vec<ManifestCapability> {
        let mut capabilities = Vec::new();

        if self.hooks.stanza_processor {
            capabilities.push(ManifestCapability::StanzaProcessor {
                priority: self.hooks.stanza_priority,
            });
        }

        if self.hooks.event_handler {
            capabilities.push(ManifestCapability::EventHandler);
        }

        if self.hooks.tui_renderer {
            capabilities.push(ManifestCapability::TuiRenderer);
        }

        if self.hooks.gui_metadata {
            capabilities.push(ManifestCapability::GuiMetadata);
        }

        if self.permissions.kv_storage {
            capabilities.push(ManifestCapability::KvStorage);
        }

        capabilities
    }

    pub fn evaluate_permissions(
        &self,
        policy: &PermissionPolicyConfig,
    ) -> Result<GrantedPermissions, PermissionPolicyError> {
        self.validate()
            .map_err(|error| PermissionPolicyError::InvalidManifest {
                plugin_id: self.id().to_string(),
                reason: error.to_string(),
            })?;

        let plugin_id = self.id();
        let overrides = policy.plugin_overrides.get(plugin_id);

        let stanza_access = resolve_boolean_permission(
            plugin_id,
            policy.mode,
            "stanza_access",
            self.permissions.stanza_access,
            overrides.map(|grant| grant.stanza_access),
        )?;

        let kv_storage = resolve_boolean_permission(
            plugin_id,
            policy.mode,
            "kv_storage",
            self.permissions.kv_storage,
            overrides.map(|grant| grant.kv_storage),
        )?;

        let event_subscriptions = resolve_event_subscriptions(
            plugin_id,
            policy.mode,
            &self.permissions.event_subscriptions,
            overrides.map(|grant| grant.event_subscriptions.as_slice()),
        )?;

        Ok(GrantedPermissions {
            stanza_access,
            event_subscriptions,
            kv_storage,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub min_waddle_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct PluginPermissions {
    #[serde(default)]
    pub stanza_access: bool,
    #[serde(default)]
    pub event_subscriptions: Vec<String>,
    #[serde(default)]
    pub kv_storage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct PluginHooks {
    #[serde(default)]
    pub stanza_processor: bool,
    #[serde(default)]
    pub stanza_priority: i32,
    #[serde(default)]
    pub event_handler: bool,
    #[serde(default)]
    pub tui_renderer: bool,
    #[serde(default)]
    pub gui_metadata: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct PluginGui {
    #[serde(default)]
    pub components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct PluginAssets {
    pub icon: Option<String>,
    pub i18n_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestCapability {
    EventHandler,
    StanzaProcessor { priority: i32 },
    TuiRenderer,
    GuiMetadata,
    KvStorage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionPolicy {
    #[default]
    Prompt,
    AllowDeclared,
    DenyAll,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PermissionGrant {
    #[serde(default)]
    pub stanza_access: bool,
    #[serde(default)]
    pub event_subscriptions: Vec<String>,
    #[serde(default)]
    pub kv_storage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionPolicyConfig {
    pub mode: PermissionPolicy,
    pub plugin_overrides: BTreeMap<String, PermissionGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GrantedPermissions {
    pub stanza_access: bool,
    pub event_subscriptions: Vec<String>,
    pub kv_storage: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to parse manifest TOML: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("failed to read manifest at {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("invalid field {field}: {reason}")]
    InvalidField { field: String, reason: String },

    #[error("invalid capability declaration: {reason}")]
    InvalidCapability { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionPolicyError {
    #[error("manifest for plugin {plugin_id} is invalid: {reason}")]
    InvalidManifest { plugin_id: String, reason: String },

    #[error("permission denied for plugin {plugin_id}: {permission} ({reason})")]
    PermissionDenied {
        plugin_id: String,
        permission: String,
        reason: String,
    },

    #[error("invalid permission override for plugin {plugin_id}: {permission} ({reason})")]
    InvalidOverride {
        plugin_id: String,
        permission: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: String,
    #[serde(default)]
    pub digest: Option<String>,
    pub installed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSummary {
    pub reference: String,
    pub name: String,
    pub description: String,
    pub latest_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFiles {
    pub manifest: PluginManifest,
    pub wasm_path: PathBuf,
    pub vue_dir: Option<PathBuf>,
    pub assets_dir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("failed to resolve reference {reference}: {reason}")]
    ResolveFailed { reference: String, reason: String },

    #[error("failed to pull {reference}: {reason}")]
    PullFailed { reference: String, reason: String },

    #[error("invalid manifest for plugin {id}: {reason}")]
    InvalidManifest { id: String, reason: String },

    #[error("signature verification failed for {reference}: {reason}")]
    SignatureVerificationFailed { reference: String, reason: String },

    #[error("plugin {id} not installed")]
    NotInstalled { id: String },

    #[error("plugin {id} already installed at version {version}")]
    AlreadyInstalled { id: String, version: String },

    #[error("registry authentication failed for {registry}: {reason}")]
    AuthenticationFailed { registry: String, reason: String },

    #[error("unsupported on this platform: {0}")]
    Unsupported(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
struct PluginIndex {
    #[serde(default)]
    plugins: Vec<InstalledPlugin>,
}

struct PulledPluginArtifact {
    manifest: PluginManifest,
    wasm_data: Vec<u8>,
    vue_data: Option<Vec<u8>>,
    assets_data: Option<Vec<u8>>,
}

pub struct PluginRegistry {
    config: RegistryConfig,
    data_dir: PathBuf,
    installed: RwLock<PluginIndex>,
}

impl PluginRegistry {
    pub fn new(config: RegistryConfig, data_dir: PathBuf) -> Result<Self, RegistryError> {
        let plugins_dir = data_dir.join("plugins");
        std::fs::create_dir_all(plugins_dir.join("cache"))?;
        std::fs::create_dir_all(plugins_dir.join("installed"))?;

        let index = load_index(&plugins_dir);

        Ok(Self {
            config,
            data_dir,
            installed: RwLock::new(index),
        })
    }

    pub fn config(&self) -> &RegistryConfig {
        &self.config
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn plugins_dir(&self) -> PathBuf {
        self.data_dir.join("plugins")
    }

    fn installed_dir(&self) -> PathBuf {
        self.plugins_dir().join("installed")
    }

    fn cache_dir(&self) -> PathBuf {
        self.plugins_dir().join("cache")
    }

    pub async fn install(&self, reference: &str) -> Result<InstalledPlugin, RegistryError> {
        if Path::new(reference).is_dir() {
            return self.install_from_local(reference).await;
        }

        self.install_from_oci(reference, false, None).await
    }

    #[cfg(feature = "native")]
    async fn install_from_oci(
        &self,
        reference: &str,
        allow_replace: bool,
        expected_plugin_id: Option<&str>,
    ) -> Result<InstalledPlugin, RegistryError> {
        let oci_ref = self.resolve_reference(reference)?;
        let ref_str = oci_ref.whole();

        info!(reference = %ref_str, "installing plugin from OCI registry");

        let client = Client::new(ClientConfig::default());
        let auth = RegistryAuth::Anonymous;

        let (manifest, digest) =
            client
                .pull_image_manifest(&oci_ref, &auth)
                .await
                .map_err(|err| RegistryError::PullFailed {
                    reference: ref_str.clone(),
                    reason: err.to_string(),
                })?;

        let artifact = self.pull_layers(&client, &oci_ref, &manifest).await?;
        let plugin_manifest = &artifact.manifest;
        let plugin_id = plugin_manifest.id().to_string();

        if let Some(expected_id) = expected_plugin_id
            && plugin_id != expected_id
        {
            return Err(RegistryError::InvalidManifest {
                id: plugin_id,
                reason: format!(
                    "artifact plugin id does not match update target (expected {expected_id})"
                ),
            });
        }

        let existing_version = {
            let index = self
                .installed
                .read()
                .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;
            index
                .plugins
                .iter()
                .find(|p| p.id == plugin_id)
                .map(|p| p.version.clone())
        };

        if let Some(version) = existing_version
            && !allow_replace
        {
            return Err(RegistryError::AlreadyInstalled {
                id: plugin_id,
                version,
            });
        }

        self.write_plugin_files(&plugin_id, &artifact)?;

        let entry = InstalledPlugin {
            id: plugin_id.clone(),
            name: plugin_manifest.name().to_string(),
            version: plugin_manifest.version().to_string(),
            source: ref_str,
            digest: Some(digest),
            installed_at: Utc::now().to_rfc3339(),
        };

        if allow_replace {
            self.upsert_index(entry.clone())?;
        } else {
            self.add_to_index(entry.clone())?;
        }

        info!(plugin_id = %plugin_id, "plugin installed");
        Ok(entry)
    }

    #[cfg(not(feature = "native"))]
    async fn install_from_oci(
        &self,
        _reference: &str,
        _allow_replace: bool,
        _expected_plugin_id: Option<&str>,
    ) -> Result<InstalledPlugin, RegistryError> {
        Err(RegistryError::Unsupported(
            "OCI plugin installation is only available with the native feature".to_string(),
        ))
    }

    async fn install_from_local(&self, path: &str) -> Result<InstalledPlugin, RegistryError> {
        let source_dir = Path::new(path);
        let manifest_path = source_dir.join("manifest.toml");
        let wasm_path = source_dir.join("plugin.wasm");

        let plugin_manifest = PluginManifest::from_path(&manifest_path)?;

        if !wasm_path.exists() {
            return Err(RegistryError::InvalidManifest {
                id: plugin_manifest.id().to_string(),
                reason: "plugin.wasm not found in source directory".to_string(),
            });
        }

        let plugin_id = plugin_manifest.id().to_string();

        {
            let index = self
                .installed
                .read()
                .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;
            if let Some(existing) = index.plugins.iter().find(|p| p.id == plugin_id) {
                return Err(RegistryError::AlreadyInstalled {
                    id: plugin_id,
                    version: existing.version.clone(),
                });
            }
        }

        let dest_dir = self.installed_dir().join(&plugin_id);
        std::fs::create_dir_all(&dest_dir)?;

        std::fs::copy(&manifest_path, dest_dir.join("manifest.toml"))?;
        std::fs::copy(&wasm_path, dest_dir.join("plugin.wasm"))?;

        let vue_src = source_dir.join("vue");
        if vue_src.is_dir() {
            copy_dir_recursive(&vue_src, &dest_dir.join("vue"))?;
        }

        let assets_src = source_dir.join("assets");
        if assets_src.is_dir() {
            copy_dir_recursive(&assets_src, &dest_dir.join("assets"))?;
        }

        let entry = InstalledPlugin {
            id: plugin_id.clone(),
            name: plugin_manifest.name().to_string(),
            version: plugin_manifest.version().to_string(),
            source: path.to_string(),
            digest: None,
            installed_at: Utc::now().to_rfc3339(),
        };

        self.add_to_index(entry.clone())?;

        info!(plugin_id = %plugin_id, source = %path, "plugin installed from local directory");
        Ok(entry)
    }

    pub async fn uninstall(&self, plugin_id: &str) -> Result<(), RegistryError> {
        {
            let index = self
                .installed
                .read()
                .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;
            if !index.plugins.iter().any(|p| p.id == plugin_id) {
                return Err(RegistryError::NotInstalled {
                    id: plugin_id.to_string(),
                });
            }
        }

        let plugin_dir = self.installed_dir().join(plugin_id);
        if plugin_dir.exists() {
            std::fs::remove_dir_all(&plugin_dir)?;
        }

        self.remove_from_index(plugin_id)?;

        info!(plugin_id = %plugin_id, "plugin uninstalled");
        Ok(())
    }

    #[cfg(feature = "native")]
    pub async fn update(&self, plugin_id: &str) -> Result<Option<InstalledPlugin>, RegistryError> {
        let (source, current_version) = {
            let index = self
                .installed
                .read()
                .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;
            let entry = index
                .plugins
                .iter()
                .find(|p| p.id == plugin_id)
                .ok_or_else(|| RegistryError::NotInstalled {
                    id: plugin_id.to_string(),
                })?;
            (entry.source.clone(), entry.version.clone())
        };

        if Path::new(&source).is_dir() {
            debug!(plugin_id = %plugin_id, "skipping update for local plugin");
            return Ok(None);
        }

        let oci_ref = self.resolve_reference(&source)?;
        let base_ref = format!("{}/{}", oci_ref.registry(), oci_ref.repository());

        let versions = self.list_versions(&base_ref).await?;

        let current =
            Version::parse(&current_version).map_err(|err| RegistryError::ResolveFailed {
                reference: source.clone(),
                reason: format!("invalid installed version: {err}"),
            })?;

        let latest = versions.iter().filter_map(|v| Version::parse(v).ok()).max();

        let Some(latest) = latest else {
            return Ok(None);
        };

        if latest <= current {
            debug!(plugin_id = %plugin_id, current = %current, "already at latest version");
            return Ok(None);
        }

        info!(
            plugin_id = %plugin_id,
            current = %current,
            latest = %latest,
            "newer version available, updating"
        );

        let new_ref = format!("{base_ref}:{latest}");
        let result = self
            .install_from_oci(&new_ref, true, Some(plugin_id))
            .await?;
        Ok(Some(result))
    }

    #[cfg(not(feature = "native"))]
    pub async fn update(&self, _plugin_id: &str) -> Result<Option<InstalledPlugin>, RegistryError> {
        Err(RegistryError::Unsupported(
            "plugin update requires native OCI registry support".to_string(),
        ))
    }

    #[cfg(feature = "native")]
    pub async fn search(
        &self,
        registry: &str,
        query: &str,
    ) -> Result<Vec<PluginSummary>, RegistryError> {
        let ref_str = if registry.contains('/') {
            registry.to_string()
        } else {
            format!("{}/{query}", self.config.default_registry)
        };

        let oci_ref: Reference = ref_str
            .parse()
            .map_err(
                |err: oci_distribution::ParseError| RegistryError::ResolveFailed {
                    reference: ref_str.clone(),
                    reason: err.to_string(),
                },
            )?;

        let client = Client::new(ClientConfig::default());
        let auth = RegistryAuth::Anonymous;

        let tag_response = client
            .list_tags(&oci_ref, &auth, None, None)
            .await
            .map_err(|err| RegistryError::ResolveFailed {
                reference: ref_str.clone(),
                reason: err.to_string(),
            })?;

        let mut summaries = Vec::new();
        for tag in &tag_response.tags {
            if !query.is_empty() && !tag.contains(query) && !tag_response.name.contains(query) {
                continue;
            }

            summaries.push(PluginSummary {
                reference: format!("{}:{tag}", ref_str),
                name: tag_response.name.clone(),
                description: String::new(),
                latest_version: tag.clone(),
            });
        }

        Ok(summaries)
    }

    #[cfg(not(feature = "native"))]
    pub async fn search(
        &self,
        _registry: &str,
        _query: &str,
    ) -> Result<Vec<PluginSummary>, RegistryError> {
        Err(RegistryError::Unsupported(
            "plugin search requires native OCI registry support".to_string(),
        ))
    }

    pub fn list_installed(&self) -> Result<Vec<InstalledPlugin>, RegistryError> {
        let index = self
            .installed
            .read()
            .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;

        Ok(index.plugins.clone())
    }

    pub fn get_plugin_files(&self, plugin_id: &str) -> Result<PluginFiles, RegistryError> {
        {
            let index = self
                .installed
                .read()
                .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;
            if !index.plugins.iter().any(|p| p.id == plugin_id) {
                return Err(RegistryError::NotInstalled {
                    id: plugin_id.to_string(),
                });
            }
        }

        let plugin_dir = self.installed_dir().join(plugin_id);
        let manifest_path = plugin_dir.join("manifest.toml");
        let wasm_path = plugin_dir.join("plugin.wasm");

        let manifest = PluginManifest::from_path(&manifest_path)?;

        let vue_dir = plugin_dir.join("vue");
        let vue_dir = if vue_dir.is_dir() {
            Some(vue_dir)
        } else {
            None
        };

        let assets_dir = plugin_dir.join("assets");
        let assets_dir = if assets_dir.is_dir() {
            Some(assets_dir)
        } else {
            None
        };

        Ok(PluginFiles {
            manifest,
            wasm_path,
            vue_dir,
            assets_dir,
        })
    }

    #[cfg(feature = "native")]
    pub async fn list_versions(&self, reference: &str) -> Result<Vec<String>, RegistryError> {
        let oci_ref = self.resolve_reference(reference)?;
        let ref_str = oci_ref.whole();

        let client = Client::new(ClientConfig::default());
        let auth = RegistryAuth::Anonymous;

        let tag_response = client
            .list_tags(&oci_ref, &auth, None, None)
            .await
            .map_err(|err| RegistryError::ResolveFailed {
                reference: ref_str.clone(),
                reason: err.to_string(),
            })?;

        let mut versions: Vec<String> = tag_response
            .tags
            .into_iter()
            .filter(|tag| Version::parse(tag).is_ok())
            .collect();

        versions.sort_by(|a, b| {
            let va = Version::parse(a).unwrap();
            let vb = Version::parse(b).unwrap();
            va.cmp(&vb)
        });

        Ok(versions)
    }

    #[cfg(not(feature = "native"))]
    pub async fn list_versions(&self, _reference: &str) -> Result<Vec<String>, RegistryError> {
        Err(RegistryError::Unsupported(
            "listing registry versions requires native OCI support".to_string(),
        ))
    }

    #[cfg(feature = "native")]
    fn resolve_reference(&self, reference: &str) -> Result<Reference, RegistryError> {
        let expanded = if reference.contains('/') {
            reference.to_string()
        } else {
            format!("{}/{reference}", self.config.default_registry)
        };

        expanded.parse().map_err(
            |err: oci_distribution::ParseError| RegistryError::ResolveFailed {
                reference: reference.to_string(),
                reason: err.to_string(),
            },
        )
    }

    #[cfg(feature = "native")]
    async fn pull_layers(
        &self,
        client: &Client,
        oci_ref: &Reference,
        manifest: &OciImageManifest,
    ) -> Result<PulledPluginArtifact, RegistryError> {
        let ref_str = oci_ref.whole();
        let mut plugin_manifest: Option<PluginManifest> = None;
        let mut wasm_data: Option<Vec<u8>> = None;
        let mut vue_data: Option<Vec<u8>> = None;
        let mut assets_data: Option<Vec<u8>> = None;

        for layer in &manifest.layers {
            let mut buf = Vec::new();

            let cache_key = layer.digest.replace(':', "-");
            let cache_path = self.cache_dir().join(&cache_key);

            if cache_path.exists() {
                debug!(digest = %layer.digest, "using cached layer");
                buf = std::fs::read(&cache_path)?;
            } else {
                debug!(digest = %layer.digest, media_type = %layer.media_type, "pulling layer");
                client
                    .pull_blob(oci_ref, layer, &mut buf)
                    .await
                    .map_err(|err| RegistryError::PullFailed {
                        reference: ref_str.clone(),
                        reason: format!("failed to pull layer {}: {err}", layer.digest),
                    })?;

                let computed_digest = format!("sha256:{:x}", Sha256::digest(&buf));
                if computed_digest != layer.digest {
                    return Err(RegistryError::PullFailed {
                        reference: ref_str.clone(),
                        reason: format!(
                            "digest mismatch for layer: expected {}, got {computed_digest}",
                            layer.digest
                        ),
                    });
                }

                std::fs::write(&cache_path, &buf)?;
            }

            match layer.media_type.as_str() {
                MEDIA_TYPE_MANIFEST => {
                    let toml_str =
                        String::from_utf8(buf).map_err(|_| RegistryError::InvalidManifest {
                            id: "unknown".to_string(),
                            reason: "manifest layer is not valid UTF-8".to_string(),
                        })?;
                    let manifest = PluginManifest::from_toml_str(&toml_str).map_err(|err| {
                        RegistryError::InvalidManifest {
                            id: "unknown".to_string(),
                            reason: err.to_string(),
                        }
                    })?;
                    plugin_manifest = Some(manifest);
                }
                MEDIA_TYPE_WASM => {
                    wasm_data = Some(buf);
                }
                MEDIA_TYPE_VUE => {
                    vue_data = Some(buf);
                }
                MEDIA_TYPE_ASSETS => {
                    assets_data = Some(buf);
                }
                other => {
                    warn!(media_type = %other, "ignoring layer with unrecognized media type");
                }
            }
        }

        let plugin_manifest = plugin_manifest.ok_or_else(|| RegistryError::InvalidManifest {
            id: "unknown".to_string(),
            reason: "no manifest layer found in OCI artifact".to_string(),
        })?;

        let plugin_id = plugin_manifest.id().to_string();
        let wasm_data = wasm_data.ok_or_else(|| RegistryError::InvalidManifest {
            id: plugin_id.clone(),
            reason: "no WASM layer found in OCI artifact".to_string(),
        })?;

        Ok(PulledPluginArtifact {
            manifest: plugin_manifest,
            wasm_data,
            vue_data,
            assets_data,
        })
    }

    fn add_to_index(&self, entry: InstalledPlugin) -> Result<(), RegistryError> {
        let mut index = self
            .installed
            .write()
            .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;
        index.plugins.push(entry);
        save_index(&self.plugins_dir(), &index)?;
        Ok(())
    }

    fn remove_from_index(&self, plugin_id: &str) -> Result<(), RegistryError> {
        let mut index = self
            .installed
            .write()
            .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;
        index.plugins.retain(|p| p.id != plugin_id);
        save_index(&self.plugins_dir(), &index)?;
        Ok(())
    }

    fn upsert_index(&self, entry: InstalledPlugin) -> Result<(), RegistryError> {
        let mut index = self
            .installed
            .write()
            .map_err(|_| RegistryError::Io(std::io::Error::other("lock poisoned")))?;
        index.plugins.retain(|plugin| plugin.id != entry.id);
        index.plugins.push(entry);
        save_index(&self.plugins_dir(), &index)?;
        Ok(())
    }

    fn write_plugin_files(
        &self,
        plugin_id: &str,
        artifact: &PulledPluginArtifact,
    ) -> Result<(), RegistryError> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let installed_dir = self.installed_dir();
        std::fs::create_dir_all(&installed_dir)?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let staging_dir = installed_dir.join(format!(".tmp-{plugin_id}-{nonce}"));

        std::fs::create_dir_all(&staging_dir)?;

        let write_result: Result<(), RegistryError> = (|| {
            let manifest_toml = toml::to_string_pretty(&artifact.manifest).map_err(|err| {
                RegistryError::InvalidManifest {
                    id: plugin_id.to_string(),
                    reason: format!("failed to serialize manifest: {err}"),
                }
            })?;
            std::fs::write(staging_dir.join("manifest.toml"), manifest_toml)?;
            std::fs::write(staging_dir.join("plugin.wasm"), &artifact.wasm_data)?;

            if let Some(vue_tar) = &artifact.vue_data {
                extract_tar(vue_tar, &staging_dir.join("vue"))?;
            }

            if let Some(assets_tar) = &artifact.assets_data {
                extract_tar(assets_tar, &staging_dir.join("assets"))?;
            }

            Ok(())
        })();

        if let Err(err) = write_result {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }

        let dest_dir = installed_dir.join(plugin_id);
        if dest_dir.exists() {
            std::fs::remove_dir_all(&dest_dir)?;
        }
        std::fs::rename(&staging_dir, &dest_dir)?;
        Ok(())
    }
}

fn load_index(plugins_dir: &Path) -> PluginIndex {
    let index_path = plugins_dir.join("index.toml");
    if !index_path.exists() {
        return PluginIndex::default();
    }

    match std::fs::read_to_string(&index_path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(err) => {
            warn!(path = %index_path.display(), %err, "failed to read plugin index, starting fresh");
            PluginIndex::default()
        }
    }
}

fn save_index(plugins_dir: &Path, index: &PluginIndex) -> Result<(), RegistryError> {
    let index_path = plugins_dir.join("index.toml");
    let contents = toml::to_string_pretty(index)
        .map_err(|err| std::io::Error::other(format!("failed to serialize index: {err}")))?;
    std::fs::write(index_path, contents)?;
    Ok(())
}

fn extract_tar(tar_data: &[u8], dest_dir: &Path) -> Result<(), RegistryError> {
    std::fs::create_dir_all(dest_dir)?;

    let mut archive = tar::Archive::new(tar_data);
    archive
        .unpack(dest_dir)
        .map_err(|err| std::io::Error::other(format!("failed to extract tar: {err}")))?;

    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let entry_path = entry.path();
        let dest_path = dst.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_recursive(&entry_path, &dest_path)?;
        } else {
            std::fs::copy(&entry_path, &dest_path)?;
        }
    }
    Ok(())
}

fn validate_plugin_metadata(metadata: &PluginMetadata) -> Result<(), ManifestError> {
    if !is_valid_plugin_id(&metadata.id) {
        return Err(ManifestError::InvalidField {
            field: "plugin.id".to_string(),
            reason: "must be a reverse-domain identifier (for example: com.waddle.omemo)"
                .to_string(),
        });
    }

    if metadata.name.trim().is_empty() {
        return Err(ManifestError::InvalidField {
            field: "plugin.name".to_string(),
            reason: "must not be empty".to_string(),
        });
    }

    if metadata.description.trim().is_empty() {
        return Err(ManifestError::InvalidField {
            field: "plugin.description".to_string(),
            reason: "must not be empty".to_string(),
        });
    }

    validate_semver("plugin.version", &metadata.version)?;

    if let Some(min_waddle_version) = &metadata.min_waddle_version {
        validate_semver("plugin.min_waddle_version", min_waddle_version)?;
    }

    Ok(())
}

fn validate_permissions(permissions: &PluginPermissions) -> Result<(), ManifestError> {
    let mut unique_patterns = BTreeSet::new();

    for pattern in &permissions.event_subscriptions {
        validate_event_pattern(pattern)?;

        if !unique_patterns.insert(pattern) {
            return Err(ManifestError::InvalidField {
                field: "permissions.event_subscriptions".to_string(),
                reason: format!("duplicate subscription pattern: {pattern}"),
            });
        }
    }

    Ok(())
}

fn validate_hooks(
    hooks: &PluginHooks,
    permissions: &PluginPermissions,
) -> Result<(), ManifestError> {
    if !hooks.stanza_processor && hooks.stanza_priority != 0 {
        return Err(ManifestError::InvalidField {
            field: "hooks.stanza_priority".to_string(),
            reason: "must be 0 when hooks.stanza_processor is false".to_string(),
        });
    }

    if hooks.stanza_processor && !permissions.stanza_access {
        return Err(ManifestError::InvalidCapability {
            reason: "hooks.stanza_processor requires permissions.stanza_access = true".to_string(),
        });
    }

    if !hooks.event_handler && !permissions.event_subscriptions.is_empty() {
        return Err(ManifestError::InvalidCapability {
            reason: "permissions.event_subscriptions requires hooks.event_handler = true"
                .to_string(),
        });
    }

    Ok(())
}

fn validate_gui(gui: &Option<PluginGui>, hooks: &PluginHooks) -> Result<(), ManifestError> {
    let Some(gui) = gui else {
        return Ok(());
    };

    if !hooks.gui_metadata {
        return Err(ManifestError::InvalidCapability {
            reason: "[gui] metadata requires hooks.gui_metadata = true".to_string(),
        });
    }

    for component in &gui.components {
        if component.trim().is_empty() {
            return Err(ManifestError::InvalidField {
                field: "gui.components".to_string(),
                reason: "component names must not be empty".to_string(),
            });
        }

        if !component.ends_with(".vue") {
            return Err(ManifestError::InvalidField {
                field: "gui.components".to_string(),
                reason: format!("component '{component}' must end with .vue"),
            });
        }
    }

    Ok(())
}

fn validate_assets(assets: &Option<PluginAssets>) -> Result<(), ManifestError> {
    let Some(assets) = assets else {
        return Ok(());
    };

    if let Some(icon) = &assets.icon
        && icon.trim().is_empty()
    {
        return Err(ManifestError::InvalidField {
            field: "assets.icon".to_string(),
            reason: "must not be empty when provided".to_string(),
        });
    }

    if let Some(i18n_dir) = &assets.i18n_dir
        && i18n_dir.trim().is_empty()
    {
        return Err(ManifestError::InvalidField {
            field: "assets.i18n_dir".to_string(),
            reason: "must not be empty when provided".to_string(),
        });
    }

    Ok(())
}

fn validate_semver(field: &str, value: &str) -> Result<(), ManifestError> {
    if Version::parse(value).is_err() {
        return Err(ManifestError::InvalidField {
            field: field.to_string(),
            reason: format!("must be valid semver (got {value})"),
        });
    }

    Ok(())
}

fn validate_event_pattern(pattern: &str) -> Result<(), ManifestError> {
    if pattern.is_empty() {
        return Err(ManifestError::InvalidField {
            field: "permissions.event_subscriptions".to_string(),
            reason: "event subscription patterns must not be empty".to_string(),
        });
    }

    Pattern::new(pattern).map_err(|error| ManifestError::InvalidField {
        field: "permissions.event_subscriptions".to_string(),
        reason: format!("invalid glob pattern '{pattern}': {error}"),
    })?;

    let first_segment = pattern.split('.').next().unwrap_or_default();
    if first_segment.is_empty() {
        return Err(ManifestError::InvalidField {
            field: "permissions.event_subscriptions".to_string(),
            reason: format!("invalid event subscription domain in pattern '{pattern}'"),
        });
    }

    if !has_glob_meta(first_segment) && !VALID_EVENT_DOMAINS.contains(&first_segment) {
        return Err(ManifestError::InvalidField {
            field: "permissions.event_subscriptions".to_string(),
            reason: format!("invalid event subscription domain in pattern '{pattern}'"),
        });
    }

    Ok(())
}

fn has_glob_meta(segment: &str) -> bool {
    segment.contains('*')
        || segment.contains('?')
        || segment.contains('[')
        || segment.contains(']')
        || segment.contains('{')
        || segment.contains('}')
        || segment.contains('!')
}

fn is_valid_plugin_id(id: &str) -> bool {
    if id.is_empty() || id.starts_with('.') || id.ends_with('.') || id.contains("..") {
        return false;
    }

    let segments: Vec<&str> = id.split('.').collect();
    if segments.len() < 3 {
        return false;
    }

    segments.iter().all(|segment| {
        !segment.is_empty()
            && !segment.starts_with('-')
            && !segment.ends_with('-')
            && segment
                .chars()
                .all(|ch| matches!(ch, 'a'..='z' | '0'..='9' | '-'))
    })
}

fn resolve_boolean_permission(
    plugin_id: &str,
    policy: PermissionPolicy,
    permission_name: &str,
    declared: bool,
    override_grant: Option<bool>,
) -> Result<bool, PermissionPolicyError> {
    if !declared {
        if override_grant == Some(true) {
            return Err(PermissionPolicyError::InvalidOverride {
                plugin_id: plugin_id.to_string(),
                permission: permission_name.to_string(),
                reason: "cannot grant a permission the manifest did not declare".to_string(),
            });
        }

        return Ok(false);
    }

    if let Some(granted) = override_grant {
        if granted {
            return Ok(true);
        }

        return Err(PermissionPolicyError::PermissionDenied {
            plugin_id: plugin_id.to_string(),
            permission: permission_name.to_string(),
            reason: "denied by per-plugin override".to_string(),
        });
    }

    match policy {
        PermissionPolicy::AllowDeclared => Ok(true),
        PermissionPolicy::DenyAll => Err(PermissionPolicyError::PermissionDenied {
            plugin_id: plugin_id.to_string(),
            permission: permission_name.to_string(),
            reason: "denied by global policy deny-all".to_string(),
        }),
        PermissionPolicy::Prompt => Err(PermissionPolicyError::PermissionDenied {
            plugin_id: plugin_id.to_string(),
            permission: permission_name.to_string(),
            reason: "requires explicit per-plugin approval under prompt policy".to_string(),
        }),
    }
}

fn resolve_event_subscriptions(
    plugin_id: &str,
    policy: PermissionPolicy,
    declared_patterns: &[String],
    override_patterns: Option<&[String]>,
) -> Result<Vec<String>, PermissionPolicyError> {
    if declared_patterns.is_empty() {
        if override_patterns.is_some_and(|patterns| !patterns.is_empty()) {
            return Err(PermissionPolicyError::InvalidOverride {
                plugin_id: plugin_id.to_string(),
                permission: "event_subscriptions".to_string(),
                reason: "cannot grant event subscriptions when none are declared".to_string(),
            });
        }

        return Ok(Vec::new());
    }

    if let Some(override_patterns) = override_patterns {
        if override_patterns.is_empty() {
            return Err(PermissionPolicyError::PermissionDenied {
                plugin_id: plugin_id.to_string(),
                permission: "event_subscriptions".to_string(),
                reason: "declared event subscriptions are denied by empty override".to_string(),
            });
        }

        for pattern in override_patterns {
            validate_event_pattern(pattern).map_err(|error| {
                PermissionPolicyError::InvalidOverride {
                    plugin_id: plugin_id.to_string(),
                    permission: "event_subscriptions".to_string(),
                    reason: error.to_string(),
                }
            })?;
        }

        for declared_pattern in declared_patterns {
            let covered = override_patterns.iter().any(|allowed_pattern| {
                Pattern::new(allowed_pattern)
                    .map(|allowed| allowed.matches(declared_pattern))
                    .unwrap_or(false)
            });

            if !covered {
                return Err(PermissionPolicyError::PermissionDenied {
                    plugin_id: plugin_id.to_string(),
                    permission: "event_subscriptions".to_string(),
                    reason: format!(
                        "declared pattern '{declared_pattern}' is not allowed by override"
                    ),
                });
            }
        }

        return Ok(declared_patterns.to_vec());
    }

    match policy {
        PermissionPolicy::AllowDeclared => Ok(declared_patterns.to_vec()),
        PermissionPolicy::DenyAll => Err(PermissionPolicyError::PermissionDenied {
            plugin_id: plugin_id.to_string(),
            permission: "event_subscriptions".to_string(),
            reason: "denied by global policy deny-all".to_string(),
        }),
        PermissionPolicy::Prompt => Err(PermissionPolicyError::PermissionDenied {
            plugin_id: plugin_id.to_string(),
            permission: "event_subscriptions".to_string(),
            reason: "requires explicit per-plugin approval under prompt policy".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_MANIFEST: &str = r#"
[plugin]
id = "com.waddle.omemo"
name = "OMEMO Encryption"
version = "1.0.0"
description = "End-to-end encryption using OMEMO (XEP-0384)"
authors = ["Waddle Team <team@waddle.social>"]
license = "MPL-2.0"
homepage = "https://github.com/waddle-social/omemo-plugin"
min_waddle_version = "0.1.0"

[permissions]
stanza_access = true
event_subscriptions = ["xmpp.message.*", "system.connection.*"]
kv_storage = true

[hooks]
stanza_processor = true
stanza_priority = -10
event_handler = true
tui_renderer = false
gui_metadata = false
"#;

    #[test]
    fn manifest_parses_and_exposes_capabilities() {
        let manifest = PluginManifest::from_toml_str(VALID_MANIFEST).unwrap();

        assert_eq!(manifest.id(), "com.waddle.omemo");
        assert_eq!(manifest.name(), "OMEMO Encryption");
        assert_eq!(manifest.version(), "1.0.0");
        assert_eq!(
            manifest.capabilities(),
            vec![
                ManifestCapability::StanzaProcessor { priority: -10 },
                ManifestCapability::EventHandler,
                ManifestCapability::KvStorage,
            ]
        );
    }

    #[test]
    fn manifest_rejects_invalid_plugin_id() {
        let manifest = VALID_MANIFEST.replace("com.waddle.omemo", "invalid");

        let error = PluginManifest::from_toml_str(&manifest).unwrap_err();
        assert!(matches!(error, ManifestError::InvalidField { field, .. } if field == "plugin.id"));
    }

    #[test]
    fn manifest_rejects_stanza_processor_without_stanza_access_permission() {
        let manifest = VALID_MANIFEST.replace("stanza_access = true", "stanza_access = false");

        let error = PluginManifest::from_toml_str(&manifest).unwrap_err();
        assert!(matches!(error, ManifestError::InvalidCapability { .. }));
    }

    #[test]
    fn manifest_rejects_event_subscriptions_without_event_handler_hook() {
        let manifest = VALID_MANIFEST.replace("event_handler = true", "event_handler = false");

        let error = PluginManifest::from_toml_str(&manifest).unwrap_err();
        assert!(matches!(error, ManifestError::InvalidCapability { .. }));
    }

    #[test]
    fn allow_declared_policy_grants_declared_permissions() {
        let manifest = PluginManifest::from_toml_str(VALID_MANIFEST).unwrap();
        let policy = PermissionPolicyConfig {
            mode: PermissionPolicy::AllowDeclared,
            plugin_overrides: BTreeMap::new(),
        };

        let granted = manifest.evaluate_permissions(&policy).unwrap();
        assert!(granted.stanza_access);
        assert!(granted.kv_storage);
        assert_eq!(
            granted.event_subscriptions,
            vec![
                "xmpp.message.*".to_string(),
                "system.connection.*".to_string()
            ]
        );
    }

    #[test]
    fn deny_all_policy_rejects_declared_permissions() {
        let manifest = PluginManifest::from_toml_str(VALID_MANIFEST).unwrap();
        let policy = PermissionPolicyConfig {
            mode: PermissionPolicy::DenyAll,
            plugin_overrides: BTreeMap::new(),
        };

        let error = manifest.evaluate_permissions(&policy).unwrap_err();
        assert!(matches!(
            error,
            PermissionPolicyError::PermissionDenied { permission, .. } if permission == "stanza_access"
        ));
    }

    #[test]
    fn prompt_policy_requires_explicit_override() {
        let manifest = PluginManifest::from_toml_str(VALID_MANIFEST).unwrap();
        let policy = PermissionPolicyConfig::default();

        let error = manifest.evaluate_permissions(&policy).unwrap_err();
        assert!(matches!(
            error,
            PermissionPolicyError::PermissionDenied { permission, .. } if permission == "stanza_access"
        ));
    }

    #[test]
    fn prompt_policy_with_override_grants_permissions() {
        let manifest = PluginManifest::from_toml_str(VALID_MANIFEST).unwrap();
        let mut plugin_overrides = BTreeMap::new();
        plugin_overrides.insert(
            "com.waddle.omemo".to_string(),
            PermissionGrant {
                stanza_access: true,
                event_subscriptions: vec![
                    "xmpp.message.*".to_string(),
                    "system.connection.*".to_string(),
                ],
                kv_storage: true,
            },
        );

        let policy = PermissionPolicyConfig {
            mode: PermissionPolicy::Prompt,
            plugin_overrides,
        };

        let granted = manifest.evaluate_permissions(&policy).unwrap();
        assert!(granted.stanza_access);
        assert!(granted.kv_storage);
        assert_eq!(
            granted.event_subscriptions,
            vec![
                "xmpp.message.*".to_string(),
                "system.connection.*".to_string(),
            ]
        );
    }

    #[test]
    fn override_must_cover_all_declared_event_subscriptions() {
        let manifest = PluginManifest::from_toml_str(VALID_MANIFEST).unwrap();
        let mut plugin_overrides = BTreeMap::new();
        plugin_overrides.insert(
            "com.waddle.omemo".to_string(),
            PermissionGrant {
                stanza_access: true,
                event_subscriptions: vec!["xmpp.message.*".to_string()],
                kv_storage: true,
            },
        );

        let policy = PermissionPolicyConfig {
            mode: PermissionPolicy::Prompt,
            plugin_overrides,
        };

        let error = manifest.evaluate_permissions(&policy).unwrap_err();
        assert!(matches!(
            error,
            PermissionPolicyError::PermissionDenied { permission, .. } if permission == "event_subscriptions"
        ));
    }

    #[test]
    fn override_cannot_grant_undeclared_permission() {
        let manifest = PluginManifest::from_toml_str(VALID_MANIFEST)
            .unwrap()
            .with_kv_storage(false);

        let mut plugin_overrides = BTreeMap::new();
        plugin_overrides.insert(
            "com.waddle.omemo".to_string(),
            PermissionGrant {
                stanza_access: true,
                event_subscriptions: vec![
                    "xmpp.message.*".to_string(),
                    "system.connection.*".to_string(),
                ],
                kv_storage: true,
            },
        );

        let policy = PermissionPolicyConfig {
            mode: PermissionPolicy::Prompt,
            plugin_overrides,
        };

        let error = manifest.evaluate_permissions(&policy).unwrap_err();
        assert!(matches!(
            error,
            PermissionPolicyError::InvalidOverride { permission, .. } if permission == "kv_storage"
        ));
    }

    #[test]
    fn registry_creates_directory_structure() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            PluginRegistry::new(RegistryConfig::default(), dir.path().to_path_buf()).unwrap();

        assert!(registry.plugins_dir().join("cache").is_dir());
        assert!(registry.plugins_dir().join("installed").is_dir());
    }

    #[test]
    fn registry_loads_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            PluginRegistry::new(RegistryConfig::default(), dir.path().to_path_buf()).unwrap();

        let installed = registry.list_installed().unwrap();
        assert!(installed.is_empty());
    }

    #[test]
    fn registry_persists_and_loads_index() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();

        {
            let registry =
                PluginRegistry::new(RegistryConfig::default(), data_dir.clone()).unwrap();
            let entry = InstalledPlugin {
                id: "com.test.plugin".to_string(),
                name: "Test Plugin".to_string(),
                version: "1.0.0".to_string(),
                source: "ghcr.io/test/plugin:1.0.0".to_string(),
                digest: Some("sha256:abc123".to_string()),
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            };
            registry.add_to_index(entry).unwrap();
        }

        {
            let registry = PluginRegistry::new(RegistryConfig::default(), data_dir).unwrap();
            let installed = registry.list_installed().unwrap();
            assert_eq!(installed.len(), 1);
            assert_eq!(installed[0].id, "com.test.plugin");
            assert_eq!(installed[0].version, "1.0.0");
        }
    }

    #[test]
    fn registry_uninstall_removes_from_index() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let registry = PluginRegistry::new(RegistryConfig::default(), data_dir).unwrap();

        let plugin_dir = registry.installed_dir().join("com.test.plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"fake").unwrap();

        let entry = InstalledPlugin {
            id: "com.test.plugin".to_string(),
            name: "Test Plugin".to_string(),
            version: "1.0.0".to_string(),
            source: "local".to_string(),
            digest: None,
            installed_at: "2026-01-01T00:00:00Z".to_string(),
        };
        registry.add_to_index(entry).unwrap();

        assert_eq!(registry.list_installed().unwrap().len(), 1);

        tokio_test::block_on(registry.uninstall("com.test.plugin")).unwrap();

        assert!(registry.list_installed().unwrap().is_empty());
        assert!(!plugin_dir.exists());
    }

    #[test]
    fn registry_uninstall_not_installed_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            PluginRegistry::new(RegistryConfig::default(), dir.path().to_path_buf()).unwrap();

        let err = tokio_test::block_on(registry.uninstall("com.test.nonexistent")).unwrap_err();
        assert!(matches!(err, RegistryError::NotInstalled { .. }));
    }

    #[tokio::test]
    async fn registry_install_from_local_directory() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let source_dir = dir.path().join("source");
        std::fs::create_dir_all(&source_dir).unwrap();

        let manifest_toml = r#"
[plugin]
id = "com.test.local"
name = "Local Plugin"
version = "0.1.0"
description = "A test plugin loaded from a local directory"

[permissions]
kv_storage = true

[hooks]
event_handler = false
"#;
        std::fs::write(source_dir.join("manifest.toml"), manifest_toml).unwrap();
        std::fs::write(source_dir.join("plugin.wasm"), b"fake-wasm-binary").unwrap();

        let registry = PluginRegistry::new(RegistryConfig::default(), data_dir).unwrap();
        let installed = registry
            .install(source_dir.to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(installed.id, "com.test.local");
        assert_eq!(installed.version, "0.1.0");
        assert!(installed.digest.is_none());

        let files = registry.get_plugin_files("com.test.local").unwrap();
        assert_eq!(files.manifest.id(), "com.test.local");
        assert!(files.wasm_path.exists());
    }

    #[tokio::test]
    async fn registry_install_local_rejects_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let source_dir = dir.path().join("source");
        std::fs::create_dir_all(&source_dir).unwrap();

        let manifest_toml = r#"
[plugin]
id = "com.test.dup"
name = "Dup Plugin"
version = "0.1.0"
description = "A duplicate test"

[permissions]

[hooks]
"#;
        std::fs::write(source_dir.join("manifest.toml"), manifest_toml).unwrap();
        std::fs::write(source_dir.join("plugin.wasm"), b"fake").unwrap();

        let registry = PluginRegistry::new(RegistryConfig::default(), data_dir).unwrap();
        registry
            .install(source_dir.to_str().unwrap())
            .await
            .unwrap();

        let err = registry
            .install(source_dir.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::AlreadyInstalled { .. }));
    }

    #[test]
    fn get_plugin_files_not_installed_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            PluginRegistry::new(RegistryConfig::default(), dir.path().to_path_buf()).unwrap();

        let err = registry.get_plugin_files("com.test.missing").unwrap_err();
        assert!(matches!(err, RegistryError::NotInstalled { .. }));
    }

    #[test]
    fn resolve_reference_expands_short_references() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            PluginRegistry::new(RegistryConfig::default(), dir.path().to_path_buf()).unwrap();

        let oci_ref = registry.resolve_reference("omemo:1.0.0").unwrap();
        assert_eq!(oci_ref.registry(), "ghcr.io");
        assert_eq!(oci_ref.repository(), "waddle-social/omemo");
        assert_eq!(oci_ref.tag(), Some("1.0.0"));
    }

    #[test]
    fn resolve_reference_keeps_full_references() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            PluginRegistry::new(RegistryConfig::default(), dir.path().to_path_buf()).unwrap();

        let oci_ref = registry
            .resolve_reference("docker.io/myorg/plugin:2.0.0")
            .unwrap();
        assert_eq!(oci_ref.registry(), "docker.io");
        assert_eq!(oci_ref.repository(), "myorg/plugin");
        assert_eq!(oci_ref.tag(), Some("2.0.0"));
    }

    #[test]
    fn plugin_index_serializes_roundtrip() {
        let index = PluginIndex {
            plugins: vec![
                InstalledPlugin {
                    id: "com.waddle.omemo".to_string(),
                    name: "OMEMO Encryption".to_string(),
                    version: "1.0.0".to_string(),
                    source: "ghcr.io/waddle-social/omemo:1.0.0".to_string(),
                    digest: Some("sha256:abcdef".to_string()),
                    installed_at: "2026-02-10T12:00:00Z".to_string(),
                },
                InstalledPlugin {
                    id: "com.example.test".to_string(),
                    name: "Test".to_string(),
                    version: "0.1.0".to_string(),
                    source: "/home/user/dev/test/".to_string(),
                    digest: None,
                    installed_at: "2026-02-10T13:00:00Z".to_string(),
                },
            ],
        };

        let serialized = toml::to_string_pretty(&index).unwrap();
        let deserialized: PluginIndex = toml::from_str(&serialized).unwrap();
        assert_eq!(index.plugins.len(), deserialized.plugins.len());
        assert_eq!(index.plugins[0].id, deserialized.plugins[0].id);
        assert_eq!(index.plugins[1].id, deserialized.plugins[1].id);
    }

    #[tokio::test]
    async fn registry_install_local_with_vue_and_assets() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let source_dir = dir.path().join("source");
        std::fs::create_dir_all(source_dir.join("vue")).unwrap();
        std::fs::create_dir_all(source_dir.join("assets/i18n")).unwrap();

        let manifest_toml = r#"
[plugin]
id = "com.test.full"
name = "Full Plugin"
version = "1.0.0"
description = "A plugin with all the bells and whistles"

[permissions]
stanza_access = true
event_subscriptions = ["xmpp.message.*"]
kv_storage = true

[hooks]
stanza_processor = true
stanza_priority = 5
event_handler = true
gui_metadata = true

[gui]
components = ["Settings.vue"]

[assets]
icon = "icon.svg"
i18n_dir = "i18n/"
"#;
        std::fs::write(source_dir.join("manifest.toml"), manifest_toml).unwrap();
        std::fs::write(source_dir.join("plugin.wasm"), b"fake-wasm").unwrap();
        std::fs::write(
            source_dir.join("vue/Settings.vue"),
            b"<template></template>",
        )
        .unwrap();
        std::fs::write(source_dir.join("assets/icon.svg"), b"<svg/>").unwrap();
        std::fs::write(source_dir.join("assets/i18n/en.json"), b"{}").unwrap();

        let registry = PluginRegistry::new(RegistryConfig::default(), data_dir).unwrap();
        let installed = registry
            .install(source_dir.to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(installed.id, "com.test.full");

        let files = registry.get_plugin_files("com.test.full").unwrap();
        assert!(files.vue_dir.is_some());
        assert!(files.assets_dir.is_some());
        assert!(files.vue_dir.unwrap().join("Settings.vue").exists());
        assert!(files.assets_dir.as_ref().unwrap().join("icon.svg").exists());
    }

    #[test]
    fn write_plugin_files_replaces_existing_plugin_directory() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            PluginRegistry::new(RegistryConfig::default(), dir.path().to_path_buf()).unwrap();
        let plugin_id = "com.test.replace";

        let initial = PulledPluginArtifact {
            manifest: test_manifest(plugin_id, "1.0.0"),
            wasm_data: b"v1".to_vec(),
            vue_data: Some(tar_bytes(&[("Settings.vue", b"<template>v1</template>")])),
            assets_data: Some(tar_bytes(&[("icon.svg", b"<svg/>")])),
        };
        registry.write_plugin_files(plugin_id, &initial).unwrap();

        let replacement = PulledPluginArtifact {
            manifest: test_manifest(plugin_id, "1.1.0"),
            wasm_data: b"v2".to_vec(),
            vue_data: None,
            assets_data: None,
        };
        registry
            .write_plugin_files(plugin_id, &replacement)
            .unwrap();

        let plugin_dir = registry.installed_dir().join(plugin_id);
        assert_eq!(
            std::fs::read(plugin_dir.join("plugin.wasm")).unwrap(),
            b"v2"
        );
        assert!(!plugin_dir.join("vue").exists());
        assert!(!plugin_dir.join("assets").exists());
    }

    #[test]
    fn write_plugin_files_failure_keeps_existing_plugin_files() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            PluginRegistry::new(RegistryConfig::default(), dir.path().to_path_buf()).unwrap();
        let plugin_id = "com.test.replace";

        let initial = PulledPluginArtifact {
            manifest: test_manifest(plugin_id, "1.0.0"),
            wasm_data: b"stable".to_vec(),
            vue_data: None,
            assets_data: None,
        };
        registry.write_plugin_files(plugin_id, &initial).unwrap();

        let broken = PulledPluginArtifact {
            manifest: test_manifest(plugin_id, "1.1.0"),
            wasm_data: b"broken".to_vec(),
            vue_data: Some(vec![0, 1, 2, 3]),
            assets_data: None,
        };
        let err = registry.write_plugin_files(plugin_id, &broken).unwrap_err();
        assert!(matches!(err, RegistryError::Io(_)));

        let plugin_dir = registry.installed_dir().join(plugin_id);
        assert_eq!(
            std::fs::read(plugin_dir.join("plugin.wasm")).unwrap(),
            b"stable"
        );
    }

    fn test_manifest(plugin_id: &str, version: &str) -> PluginManifest {
        let manifest_toml = format!(
            r#"
[plugin]
id = "{plugin_id}"
name = "Test Plugin"
version = "{version}"
description = "Test plugin manifest"

[permissions]

[hooks]
"#
        );
        PluginManifest::from_toml_str(&manifest_toml).unwrap()
    }

    fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            for (path, contents) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(0);
                header.set_cksum();
                builder.append_data(&mut header, *path, *contents).unwrap();
            }
            builder.finish().unwrap();
        }
        bytes
    }

    impl PluginManifest {
        fn with_kv_storage(mut self, kv_storage: bool) -> Self {
            self.permissions.kv_storage = kv_storage;
            self
        }
    }
}
