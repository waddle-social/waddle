use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration file not found at {path}")]
    FileNotFound { path: PathBuf },

    #[error("invalid TOML at line {line}, column {column}: {message}")]
    InvalidToml {
        line: usize,
        column: usize,
        message: String,
    },

    #[error("missing required fields: {fields:?}")]
    MissingRequiredFields { fields: Vec<String> },

    #[error("invalid value for {field}: {message}")]
    InvalidValue { field: String, message: String },

    #[error("I/O error reading configuration: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub account: AccountConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub theme: ThemeConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub event_bus: EventBusConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountConfig {
    pub jid: String,
    pub password: String,
    pub server: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_true")]
    pub notifications: bool,
    #[serde(default = "default_theme_name")]
    pub theme: String,
    pub locale: Option<String>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            notifications: true,
            theme: "default".to_string(),
            locale: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThemeConfig {
    #[serde(default = "default_theme_name")]
    pub name: String,
    pub custom_path: Option<String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            custom_path: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub directory: Option<String>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventBusConfig {
    #[serde(default = "default_channel_capacity")]
    pub channel_capacity: usize,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 1024,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StorageConfig {
    pub path: Option<String>,
}

#[derive(Debug, Default, Clone)]
struct ConfigOverrides {
    jid: Option<String>,
    password: Option<String>,
    server: Option<String>,
    log_level: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_theme_name() -> String {
    "default".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_channel_capacity() -> usize {
    1024
}

const VALID_LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];

const DEFAULT_CONFIG_TOML: &str = r#"[account]
jid = ""
password = ""
# server = "xmpp.example.com"
# port = 5222

[ui]
notifications = true
theme = "default"

[theme]
name = "default"
# custom_path = "/path/to/custom/theme.toml"

[plugins]
enabled = true
# directory = "~/.local/share/waddle/plugins"

[logging]
level = "info"

[event_bus]
channel_capacity = 1024

[storage]
# path = "~/.local/share/waddle/waddle.db"
"#;

/// Return the resolved platform-appropriate configuration file path.
#[cfg(feature = "native")]
pub fn config_path() -> PathBuf {
    if let Some(proj_dirs) = directories::ProjectDirs::from("com", "waddle", "waddle") {
        proj_dirs.config_dir().join("config.toml")
    } else {
        PathBuf::from("config.toml")
    }
}

/// Load configuration from the platform config path, merging environment
/// variable overrides. Returns a validated Config or a descriptive error.
#[cfg(feature = "native")]
pub fn load_config() -> Result<Config, ConfigError> {
    load_config_from(config_path())
}

/// Load configuration from a specific path. Used by `load_config()` and tests.
pub fn load_config_from(path: PathBuf) -> Result<Config, ConfigError> {
    load_config_from_with_overrides(path, config_overrides_from_env())
}

/// Parse configuration from a TOML string directly (for testing).
pub fn load_config_from_str(toml_str: &str) -> Result<Config, ConfigError> {
    load_config_from_str_with_overrides(toml_str, config_overrides_from_env())
}

fn load_config_from_with_overrides(
    path: PathBuf,
    overrides: ConfigOverrides,
) -> Result<Config, ConfigError> {
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_default_config(&path)?;
            return Err(ConfigError::MissingRequiredFields {
                fields: vec!["account.jid".to_string(), "account.password".to_string()],
            });
        }
        Err(e) => return Err(ConfigError::Io(e)),
    };

    load_config_from_str_with_overrides(&contents, overrides)
}

fn load_config_from_str_with_overrides(
    toml_str: &str,
    overrides: ConfigOverrides,
) -> Result<Config, ConfigError> {
    let mut config: Config = toml::from_str(toml_str).map_err(|e| {
        let (line, column) = e.span().map_or((0, 0), |span| {
            let before = &toml_str[..span.start];
            let line = before.chars().filter(|&c| c == '\n').count() + 1;
            let column = before
                .rfind('\n')
                .map_or(span.start + 1, |nl| span.start - nl);
            (line, column)
        });
        ConfigError::InvalidToml {
            line,
            column,
            message: e.message().to_string(),
        }
    })?;

    apply_overrides(&mut config, overrides);
    validate(&config)?;

    Ok(config)
}

fn config_overrides_from_env() -> ConfigOverrides {
    ConfigOverrides {
        jid: std::env::var("WADDLE_JID").ok(),
        password: std::env::var("WADDLE_PASSWORD").ok(),
        server: std::env::var("WADDLE_SERVER").ok(),
        log_level: std::env::var("WADDLE_LOG_LEVEL").ok(),
    }
}

fn apply_overrides(config: &mut Config, overrides: ConfigOverrides) {
    if let Some(jid) = overrides.jid {
        config.account.jid = jid;
    }
    if let Some(password) = overrides.password {
        config.account.password = password;
    }
    if let Some(server) = overrides.server {
        config.account.server = Some(server);
    }
    if let Some(level) = overrides.log_level {
        config.logging.level = level;
    }
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    let mut missing = Vec::new();

    if config.account.jid.is_empty() {
        missing.push("account.jid".to_string());
    }
    if config.account.password.is_empty() {
        missing.push("account.password".to_string());
    }

    if !missing.is_empty() {
        return Err(ConfigError::MissingRequiredFields { fields: missing });
    }

    if !VALID_LOG_LEVELS.contains(&config.logging.level.as_str()) {
        return Err(ConfigError::InvalidValue {
            field: "logging.level".to_string(),
            message: format!("must be one of: {}", VALID_LOG_LEVELS.join(", ")),
        });
    }

    Ok(())
}

fn create_default_config(path: &PathBuf) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, DEFAULT_CONFIG_TOML)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_without_env(toml_str: &str) -> Result<Config, ConfigError> {
        load_config_from_str_with_overrides(toml_str, ConfigOverrides::default())
    }

    fn valid_toml() -> &'static str {
        r#"
[account]
jid = "user@example.com"
password = "secret"

[ui]
notifications = true
theme = "default"

[theme]
name = "default"

[plugins]
enabled = true

[logging]
level = "info"

[event_bus]
channel_capacity = 1024

[storage]
"#
    }

    fn minimal_toml() -> &'static str {
        r#"
[account]
jid = "user@example.com"
password = "secret"
"#
    }

    // ── Round-trip parsing ────────────────────────────────────────

    #[test]
    fn parses_full_config() {
        let config = parse_without_env(valid_toml()).unwrap();
        assert_eq!(config.account.jid, "user@example.com");
        assert_eq!(config.account.password, "secret");
        assert!(config.account.server.is_none());
        assert!(config.account.port.is_none());
        assert!(config.ui.notifications);
        assert_eq!(config.ui.theme, "default");
        assert_eq!(config.theme.name, "default");
        assert!(config.theme.custom_path.is_none());
        assert!(config.plugins.enabled);
        assert!(config.plugins.directory.is_none());
        assert_eq!(config.logging.level, "info");
        assert_eq!(config.event_bus.channel_capacity, 1024);
        assert!(config.storage.path.is_none());
    }

    #[test]
    fn parses_minimal_config_with_defaults() {
        let config = parse_without_env(minimal_toml()).unwrap();
        assert_eq!(config.account.jid, "user@example.com");
        assert_eq!(config.account.password, "secret");
        assert!(config.ui.notifications);
        assert_eq!(config.ui.theme, "default");
        assert_eq!(config.theme.name, "default");
        assert!(config.plugins.enabled);
        assert_eq!(config.logging.level, "info");
        assert_eq!(config.event_bus.channel_capacity, 1024);
    }

    #[test]
    fn parses_optional_account_fields() {
        let toml = r#"
[account]
jid = "user@example.com"
password = "secret"
server = "xmpp.example.com"
port = 5222
"#;
        let config = parse_without_env(toml).unwrap();
        assert_eq!(config.account.server.as_deref(), Some("xmpp.example.com"));
        assert_eq!(config.account.port, Some(5222));
    }

    #[test]
    fn parses_custom_theme_path() {
        let toml = r#"
[account]
jid = "user@example.com"
password = "secret"

[theme]
name = "dracula"
custom_path = "/home/user/.config/waddle/themes/dracula.toml"
"#;
        let config = parse_without_env(toml).unwrap();
        assert_eq!(config.theme.name, "dracula");
        assert_eq!(
            config.theme.custom_path.as_deref(),
            Some("/home/user/.config/waddle/themes/dracula.toml")
        );
    }

    #[test]
    fn parses_custom_plugin_directory() {
        let toml = r#"
[account]
jid = "user@example.com"
password = "secret"

[plugins]
enabled = false
directory = "/opt/waddle/plugins"
"#;
        let config = parse_without_env(toml).unwrap();
        assert!(!config.plugins.enabled);
        assert_eq!(
            config.plugins.directory.as_deref(),
            Some("/opt/waddle/plugins")
        );
    }

    #[test]
    fn parses_custom_storage_path() {
        let toml = r#"
[account]
jid = "user@example.com"
password = "secret"

[storage]
path = "/data/waddle.db"
"#;
        let config = parse_without_env(toml).unwrap();
        assert_eq!(config.storage.path.as_deref(), Some("/data/waddle.db"));
    }

    // ── Validation ────────────────────────────────────────────────

    #[test]
    fn rejects_missing_jid() {
        let toml = r#"
[account]
jid = ""
password = "secret"
"#;
        let err = parse_without_env(toml).unwrap_err();
        match err {
            ConfigError::MissingRequiredFields { fields } => {
                assert!(fields.contains(&"account.jid".to_string()));
                assert!(!fields.contains(&"account.password".to_string()));
            }
            other => panic!("expected MissingRequiredFields, got: {other}"),
        }
    }

    #[test]
    fn rejects_missing_password() {
        let toml = r#"
[account]
jid = "user@example.com"
password = ""
"#;
        let err = parse_without_env(toml).unwrap_err();
        match err {
            ConfigError::MissingRequiredFields { fields } => {
                assert!(fields.contains(&"account.password".to_string()));
                assert!(!fields.contains(&"account.jid".to_string()));
            }
            other => panic!("expected MissingRequiredFields, got: {other}"),
        }
    }

    #[test]
    fn rejects_both_missing() {
        let toml = r#"
[account]
jid = ""
password = ""
"#;
        let err = parse_without_env(toml).unwrap_err();
        match err {
            ConfigError::MissingRequiredFields { fields } => {
                assert_eq!(fields.len(), 2);
                assert!(fields.contains(&"account.jid".to_string()));
                assert!(fields.contains(&"account.password".to_string()));
            }
            other => panic!("expected MissingRequiredFields, got: {other}"),
        }
    }

    #[test]
    fn rejects_invalid_log_level() {
        let toml = r#"
[account]
jid = "user@example.com"
password = "secret"

[logging]
level = "verbose"
"#;
        let err = parse_without_env(toml).unwrap_err();
        match err {
            ConfigError::InvalidValue { field, .. } => {
                assert_eq!(field, "logging.level");
            }
            other => panic!("expected InvalidValue, got: {other}"),
        }
    }

    #[test]
    fn accepts_all_valid_log_levels() {
        for level in VALID_LOG_LEVELS {
            let toml = format!(
                r#"
[account]
jid = "user@example.com"
password = "secret"

[logging]
level = "{level}"
"#
            );
            parse_without_env(&toml).unwrap();
        }
    }

    // ── Invalid TOML ──────────────────────────────────────────────

    #[test]
    fn rejects_invalid_toml_syntax() {
        let toml = r#"
[account
jid = "broken"
"#;
        let err = parse_without_env(toml).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToml { .. }));
    }

    #[test]
    fn invalid_toml_reports_position() {
        let toml = r#"
[account]
jid = "user@example.com"
password = "secret"
bad_line ===
"#;
        let err = parse_without_env(toml).unwrap_err();
        match err {
            ConfigError::InvalidToml { line, .. } => {
                assert!(line > 0, "line should be > 0, got {line}");
            }
            other => panic!("expected InvalidToml, got: {other}"),
        }
    }

    // ── Environment variable overrides ────────────────────────────

    #[test]
    fn env_override_jid() {
        let overrides = ConfigOverrides {
            jid: Some("override@example.com".to_string()),
            ..Default::default()
        };
        let config = load_config_from_str_with_overrides(minimal_toml(), overrides).unwrap();
        assert_eq!(config.account.jid, "override@example.com");
    }

    #[test]
    fn env_override_password() {
        let overrides = ConfigOverrides {
            password: Some("env_password".to_string()),
            ..Default::default()
        };
        let config = load_config_from_str_with_overrides(minimal_toml(), overrides).unwrap();
        assert_eq!(config.account.password, "env_password");
    }

    #[test]
    fn env_override_server() {
        let overrides = ConfigOverrides {
            server: Some("env.xmpp.example.com".to_string()),
            ..Default::default()
        };
        let config = load_config_from_str_with_overrides(minimal_toml(), overrides).unwrap();
        assert_eq!(
            config.account.server.as_deref(),
            Some("env.xmpp.example.com")
        );
    }

    #[test]
    fn env_override_log_level() {
        let overrides = ConfigOverrides {
            log_level: Some("debug".to_string()),
            ..Default::default()
        };
        let config = load_config_from_str_with_overrides(minimal_toml(), overrides).unwrap();
        assert_eq!(config.logging.level, "debug");
    }

    #[test]
    fn env_override_invalid_log_level_rejected() {
        let overrides = ConfigOverrides {
            log_level: Some("invalid".to_string()),
            ..Default::default()
        };
        let err = load_config_from_str_with_overrides(minimal_toml(), overrides).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidValue { .. }));
    }

    #[test]
    fn env_overrides_take_precedence() {
        let toml = r#"
[account]
jid = "file@example.com"
password = "file_password"
server = "file.xmpp.example.com"

[logging]
level = "warn"
"#;
        let overrides = ConfigOverrides {
            jid: Some("env@example.com".to_string()),
            password: Some("env_password".to_string()),
            server: Some("env.xmpp.example.com".to_string()),
            log_level: Some("trace".to_string()),
        };

        let config = load_config_from_str_with_overrides(toml, overrides).unwrap();
        assert_eq!(config.account.jid, "env@example.com");
        assert_eq!(config.account.password, "env_password");
        assert_eq!(
            config.account.server.as_deref(),
            Some("env.xmpp.example.com")
        );
        assert_eq!(config.logging.level, "trace");
    }

    // ── File-based loading ────────────────────────────────────────

    #[test]
    fn load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, minimal_toml()).unwrap();

        let config = load_config_from_with_overrides(path, ConfigOverrides::default()).unwrap();
        assert_eq!(config.account.jid, "user@example.com");
    }

    #[test]
    fn missing_file_creates_default_and_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("config.toml");

        let err =
            load_config_from_with_overrides(path.clone(), ConfigOverrides::default()).unwrap_err();
        match err {
            ConfigError::MissingRequiredFields { fields } => {
                assert!(fields.contains(&"account.jid".to_string()));
                assert!(fields.contains(&"account.password".to_string()));
            }
            other => panic!("expected MissingRequiredFields, got: {other}"),
        }

        assert!(path.exists(), "default config should have been created");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[account]"));
    }

    // ── config_path ───────────────────────────────────────────────

    #[cfg(feature = "native")]
    #[test]
    fn config_path_ends_with_config_toml() {
        let path = config_path();
        assert!(
            path.ends_with("config.toml"),
            "config_path should end with config.toml, got: {path:?}"
        );
    }
}
