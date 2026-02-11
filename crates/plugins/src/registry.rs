use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use glob::Pattern;
use semver::Version;

const VALID_EVENT_DOMAINS: &[&str] = &["system", "xmpp", "ui", "plugin"];

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: String,
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

    #[error("plugin registry is not implemented")]
    NotImplemented,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct PluginRegistry {
    config: RegistryConfig,
    data_dir: PathBuf,
    installed: RwLock<Vec<InstalledPlugin>>,
}

impl PluginRegistry {
    pub fn new(config: RegistryConfig, data_dir: PathBuf) -> Result<Self, RegistryError> {
        std::fs::create_dir_all(data_dir.join("plugins"))?;

        Ok(Self {
            config,
            data_dir,
            installed: RwLock::new(Vec::new()),
        })
    }

    pub fn config(&self) -> &RegistryConfig {
        &self.config
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub async fn install(&self, _reference: &str) -> Result<InstalledPlugin, RegistryError> {
        Err(RegistryError::NotImplemented)
    }

    pub async fn uninstall(&self, _plugin_id: &str) -> Result<(), RegistryError> {
        Err(RegistryError::NotImplemented)
    }

    pub async fn update(&self, _plugin_id: &str) -> Result<Option<InstalledPlugin>, RegistryError> {
        Err(RegistryError::NotImplemented)
    }

    pub async fn search(
        &self,
        _registry: &str,
        _query: &str,
    ) -> Result<Vec<PluginSummary>, RegistryError> {
        Err(RegistryError::NotImplemented)
    }

    pub fn list_installed(&self) -> Result<Vec<InstalledPlugin>, RegistryError> {
        let installed = self
            .installed
            .read()
            .map_err(|_| RegistryError::NotImplemented)?;

        Ok(installed.clone())
    }

    pub fn get_plugin_files(&self, _plugin_id: &str) -> Result<PluginFiles, RegistryError> {
        Err(RegistryError::NotImplemented)
    }

    pub async fn list_versions(&self, _reference: &str) -> Result<Vec<String>, RegistryError> {
        Err(RegistryError::NotImplemented)
    }
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

    impl PluginManifest {
        fn with_kv_storage(mut self, kv_storage: bool) -> Self {
            self.permissions.kv_storage = kv_storage;
            self
        }
    }
}
