pub mod kv;
pub mod registry;
pub mod runtime;

pub use kv::{KvError, KvQuota, KvUsage, PluginKvStore};
pub use registry::{
    GrantedPermissions, InstalledPlugin, ManifestCapability, ManifestError, PermissionGrant,
    PermissionPolicy, PermissionPolicyConfig, PermissionPolicyError, PluginAssets, PluginFiles,
    PluginGui, PluginHooks, PluginManifest, PluginMetadata, PluginPermissions, PluginRegistry,
    PluginSummary, RegistryConfig, RegistryError,
};
pub use runtime::{
    PluginCapability, PluginError, PluginHandle, PluginHook, PluginInfo, PluginRuntime,
    PluginRuntimeConfig, PluginStatus,
};
