// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Configuration management for the Waddle TUI.
//!
//! Configuration is loaded from standard directories:
//! - `~/.config/waddle/config.toml` - Main configuration
//! - `~/.local/share/waddle/` - Data storage (credentials, etc.)
//! - `~/.cache/waddle/` - Cache files

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Color theme configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// Primary accent color (hex)
    pub accent: String,
    /// Background color (hex or "default")
    pub background: String,
    /// Foreground/text color (hex or "default")
    pub foreground: String,
    /// Border color (hex or "default")
    pub border: String,
    /// Selected item highlight color
    pub selection: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            accent: "#ff6b6b".into(), // Waddle coral
            background: "default".into(),
            foreground: "default".into(),
            border: "#444444".into(),
            selection: "#3d3d3d".into(),
        }
    }
}

/// Keybinding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    /// Enable vim-style navigation (j/k)
    pub vim_mode: bool,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self { vim_mode: true }
    }
}

/// XMPP connection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct XmppConfig {
    /// XMPP JID (e.g., user@example.com)
    pub jid: Option<String>,
    /// XMPP server host (if different from JID domain)
    pub server: Option<String>,
    /// Port (default 5222)
    pub port: u16,
    /// Whether to use TLS (STARTTLS)
    pub use_tls: bool,
    /// Session token for SASL PLAIN authentication
    /// This is obtained from the API after login
    #[serde(skip_serializing)]
    pub token: Option<String>,
    /// MUC (Multi-User Chat) domain (e.g., muc.waddle.social)
    pub muc_domain: Option<String>,
}

impl Default for XmppConfig {
    fn default() -> Self {
        Self {
            jid: None,
            server: None,
            port: 5222,
            use_tls: true,
            token: None,
            muc_domain: None,
        }
    }
}

/// UI configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Sidebar width (in characters)
    pub sidebar_width: u16,
    /// Show timestamps in messages
    pub show_timestamps: bool,
    /// Time format for timestamps
    pub time_format: String,
    /// Show user avatars (when available)
    pub show_avatars: bool,
    /// Enable mouse support
    pub mouse_support: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 24,
            show_timestamps: true,
            time_format: "%H:%M".into(),
            show_avatars: false,
            mouse_support: true,
        }
    }
}

/// Main configuration struct
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Theme configuration
    pub theme: ThemeConfig,
    /// Keybindings configuration
    pub keybindings: KeybindingsConfig,
    /// XMPP configuration
    pub xmpp: XmppConfig,
    /// UI configuration
    pub ui: UiConfig,
}

impl Config {
    /// Load configuration from XDG config directory
    pub fn load() -> Result<Self> {
        let config_path = Self::config_file_path()?;

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read config file: {:?}", config_path))?;
            let config: Config = toml::from_str(&content)
                .with_context(|| format!("Failed to parse config file: {:?}", config_path))?;
            tracing::info!("Loaded configuration from {:?}", config_path);
            Ok(config)
        } else {
            tracing::info!("No config file found, using defaults");
            Ok(Config::default())
        }
    }

    /// Save configuration to XDG config directory
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_file_path()?;

        // Ensure directory exists
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
        }

        let content = toml::to_string_pretty(self).context("Failed to serialize configuration")?;
        std::fs::write(&config_path, content)
            .with_context(|| format!("Failed to write config file: {:?}", config_path))?;

        tracing::info!("Saved configuration to {:?}", config_path);
        Ok(())
    }

    /// Get the path to the config file
    pub fn config_file_path() -> Result<PathBuf> {
        let config_dir = Self::config_dir()?;
        Ok(config_dir.join("config.toml"))
    }

    /// Get the config directory path (~/.config/waddle/)
    pub fn config_dir() -> Result<PathBuf> {
        let base = dirs::config_dir().context("Failed to determine config directory")?;
        let path = base.join("waddle");
        std::fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create config directory: {:?}", path))?;
        Ok(path)
    }

    /// Get the data directory path (~/.local/share/waddle/)
    pub fn data_dir() -> Result<PathBuf> {
        let base = dirs::data_dir().context("Failed to determine data directory")?;
        let path = base.join("waddle");
        std::fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create data directory: {:?}", path))?;
        Ok(path)
    }

    /// Get the cache directory path (~/.cache/waddle/)
    pub fn cache_dir() -> Result<PathBuf> {
        let base = dirs::cache_dir().context("Failed to determine cache directory")?;
        let path = base.join("waddle");
        std::fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create cache directory: {:?}", path))?;
        Ok(path)
    }

    /// Create a default config file if it doesn't exist
    pub fn create_default_if_missing() -> Result<bool> {
        let config_path = Self::config_file_path()?;
        if !config_path.exists() {
            let config = Config::default();
            config.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.keybindings.vim_mode);
        assert_eq!(config.ui.sidebar_width, 24);
        assert_eq!(config.xmpp.port, 5222);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml).unwrap();
        assert_eq!(config.ui.sidebar_width, parsed.ui.sidebar_width);
    }
}
