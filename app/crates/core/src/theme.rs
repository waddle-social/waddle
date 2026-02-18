use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::config::ThemeConfig;

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme not found: {0}")]
    NotFound(String),

    #[error("failed to parse custom theme at {path}: {reason}")]
    ParseFailed { path: String, reason: String },

    #[error("invalid color value: {0}")]
    InvalidColor(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThemeColors {
    pub background: String,
    pub foreground: String,
    pub surface: String,
    pub accent: String,
    pub border: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub muted: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TuiOverrides {
    pub roster_highlight: Option<String>,
    pub status_bar_bg: Option<String>,
    pub input_border: Option<String>,
    pub unread_badge: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuiOverrides {
    pub border_radius: Option<String>,
    pub font_family: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub colors: ThemeColors,
    pub tui_overrides: Option<TuiOverrides>,
    pub gui_overrides: Option<GuiOverrides>,
    plugin_colors: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct ThemeFile {
    meta: ThemeFileMeta,
    colors: ThemeColors,
    tui: Option<TuiOverrides>,
    gui: Option<GuiOverrides>,
}

#[derive(Debug, Deserialize)]
struct ThemeFileMeta {
    name: String,
    _description: Option<String>,
}

impl Theme {
    pub fn css_custom_properties(&self) -> HashMap<String, String> {
        let mut props = HashMap::new();
        props.insert("--waddle-bg".to_string(), self.colors.background.clone());
        props.insert("--waddle-fg".to_string(), self.colors.foreground.clone());
        props.insert("--waddle-surface".to_string(), self.colors.surface.clone());
        props.insert("--waddle-accent".to_string(), self.colors.accent.clone());
        props.insert("--waddle-border".to_string(), self.colors.border.clone());
        props.insert("--waddle-success".to_string(), self.colors.success.clone());
        props.insert("--waddle-warning".to_string(), self.colors.warning.clone());
        props.insert("--waddle-error".to_string(), self.colors.error.clone());
        props.insert("--waddle-muted".to_string(), self.colors.muted.clone());

        if let Some(gui) = &self.gui_overrides {
            if let Some(radius) = &gui.border_radius {
                props.insert("--waddle-border-radius".to_string(), radius.clone());
            }
            if let Some(font) = &gui.font_family {
                props.insert("--waddle-font-family".to_string(), font.clone());
            }
        }

        for (plugin_id, tokens) in &self.plugin_colors {
            for (token, value) in tokens {
                props.insert(
                    format!("--waddle-plugin-{plugin_id}-{token}"),
                    value.clone(),
                );
            }
        }

        props
    }

    pub fn register_plugin_colors(
        &mut self,
        plugin_id: &str,
        colors: HashMap<String, String>,
    ) -> Result<(), ThemeError> {
        for value in colors.values() {
            validate_color(value)?;
        }

        self.plugin_colors.insert(plugin_id.to_string(), colors);
        Ok(())
    }

    pub fn plugin_color(&self, plugin_id: &str, token: &str) -> Option<String> {
        self.plugin_colors
            .get(plugin_id)
            .and_then(|tokens| tokens.get(token))
            .cloned()
    }
}

fn validate_color(value: &str) -> Result<(), ThemeError> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed.strip_prefix('#') {
        if (hex.len() == 3 || hex.len() == 6 || hex.len() == 8)
            && hex.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Ok(());
        }
    }
    Err(ThemeError::InvalidColor(value.to_string()))
}

fn validate_theme_colors(colors: &ThemeColors) -> Result<(), ThemeError> {
    validate_color(&colors.background)?;
    validate_color(&colors.foreground)?;
    validate_color(&colors.surface)?;
    validate_color(&colors.accent)?;
    validate_color(&colors.border)?;
    validate_color(&colors.success)?;
    validate_color(&colors.warning)?;
    validate_color(&colors.error)?;
    validate_color(&colors.muted)?;
    Ok(())
}

fn builtin_default() -> Theme {
    Theme {
        name: "default".to_string(),
        colors: ThemeColors {
            background: "#ffffff".to_string(),
            foreground: "#1a1a1a".to_string(),
            surface: "#f5f5f5".to_string(),
            accent: "#0066cc".to_string(),
            border: "#d4d4d4".to_string(),
            success: "#22863a".to_string(),
            warning: "#b08800".to_string(),
            error: "#cb2431".to_string(),
            muted: "#6a737d".to_string(),
        },
        tui_overrides: None,
        gui_overrides: None,
        plugin_colors: HashMap::new(),
    }
}

fn builtin_dark() -> Theme {
    Theme {
        name: "dark".to_string(),
        colors: ThemeColors {
            background: "#1e1e2e".to_string(),
            foreground: "#cdd6f4".to_string(),
            surface: "#313244".to_string(),
            accent: "#89b4fa".to_string(),
            border: "#45475a".to_string(),
            success: "#a6e3a1".to_string(),
            warning: "#f9e2af".to_string(),
            error: "#f38ba8".to_string(),
            muted: "#6c7086".to_string(),
        },
        tui_overrides: None,
        gui_overrides: None,
        plugin_colors: HashMap::new(),
    }
}

fn builtin_high_contrast() -> Theme {
    Theme {
        name: "high-contrast".to_string(),
        colors: ThemeColors {
            background: "#000000".to_string(),
            foreground: "#ffffff".to_string(),
            surface: "#1a1a1a".to_string(),
            accent: "#ffff00".to_string(),
            border: "#ffffff".to_string(),
            success: "#00ff00".to_string(),
            warning: "#ffff00".to_string(),
            error: "#ff0000".to_string(),
            muted: "#aaaaaa".to_string(),
        },
        tui_overrides: None,
        gui_overrides: None,
        plugin_colors: HashMap::new(),
    }
}

const BUILTIN_NAMES: &[&str] = &["default", "dark", "high-contrast"];

pub struct ThemeManager {
    custom_themes: HashMap<String, Theme>,
}

impl ThemeManager {
    pub fn load(config: &ThemeConfig) -> Result<Theme, ThemeError> {
        if let Some(ref custom_path) = config.custom_path {
            match Self::load_custom(custom_path) {
                Ok(theme) => return Ok(theme),
                Err(err) => {
                    tracing::warn!(
                        "failed to load custom theme from '{}': {}; falling back to default",
                        custom_path,
                        err
                    );
                    return Ok(builtin_default());
                }
            }
        }

        if let Some(theme) = Self::builtin(&config.name) {
            return Ok(theme);
        }

        tracing::warn!("theme '{}' not found, falling back to default", config.name);
        Ok(builtin_default())
    }

    pub fn builtin(name: &str) -> Option<Theme> {
        match name {
            "default" => Some(builtin_default()),
            "dark" => Some(builtin_dark()),
            "high-contrast" => Some(builtin_high_contrast()),
            _ => None,
        }
    }

    pub fn new() -> Self {
        Self {
            custom_themes: HashMap::new(),
        }
    }

    pub fn register_custom(&mut self, theme: Theme) {
        self.custom_themes.insert(theme.name.clone(), theme);
    }

    pub fn available_themes(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILTIN_NAMES.iter().map(|s| s.to_string()).collect();
        for key in self.custom_themes.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
        names.sort();
        names
    }

    pub fn get(&self, name: &str) -> Option<Theme> {
        Self::builtin(name).or_else(|| self.custom_themes.get(name).cloned())
    }

    fn load_custom(path: &str) -> Result<Theme, ThemeError> {
        let path_ref = Path::new(path);
        let content = std::fs::read_to_string(path_ref).map_err(|e| ThemeError::ParseFailed {
            path: path.to_string(),
            reason: e.to_string(),
        })?;

        let theme_file: ThemeFile =
            toml::from_str(&content).map_err(|e| ThemeError::ParseFailed {
                path: path.to_string(),
                reason: e.to_string(),
            })?;

        validate_theme_colors(&theme_file.colors)?;

        Ok(Theme {
            name: theme_file.meta.name,
            colors: theme_file.colors,
            tui_overrides: theme_file.tui,
            gui_overrides: theme_file.gui,
            plugin_colors: HashMap::new(),
        })
    }
}

impl Default for ThemeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_theme() {
        let theme = ThemeManager::builtin("default").unwrap();
        assert_eq!(theme.name, "default");
        assert_eq!(theme.colors.background, "#ffffff");
        assert_eq!(theme.colors.foreground, "#1a1a1a");
    }

    #[test]
    fn builtin_dark_theme() {
        let theme = ThemeManager::builtin("dark").unwrap();
        assert_eq!(theme.name, "dark");
        assert_eq!(theme.colors.background, "#1e1e2e");
    }

    #[test]
    fn builtin_high_contrast_theme() {
        let theme = ThemeManager::builtin("high-contrast").unwrap();
        assert_eq!(theme.name, "high-contrast");
        assert_eq!(theme.colors.background, "#000000");
        assert_eq!(theme.colors.foreground, "#ffffff");
    }

    #[test]
    fn builtin_returns_none_for_unknown() {
        assert!(ThemeManager::builtin("nonexistent").is_none());
    }

    #[test]
    fn load_default_from_config() {
        let config = ThemeConfig::default();
        let theme = ThemeManager::load(&config).unwrap();
        assert_eq!(theme.name, "default");
    }

    #[test]
    fn load_dark_from_config() {
        let config = ThemeConfig {
            name: "dark".to_string(),
            custom_path: None,
        };
        let theme = ThemeManager::load(&config).unwrap();
        assert_eq!(theme.name, "dark");
    }

    #[test]
    fn load_falls_back_to_default_for_unknown_name() {
        let config = ThemeConfig {
            name: "nonexistent".to_string(),
            custom_path: None,
        };
        let theme = ThemeManager::load(&config).unwrap();
        assert_eq!(theme.name, "default");
    }

    #[test]
    fn load_custom_theme_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("custom.toml");
        std::fs::write(
            &theme_path,
            r##"
[meta]
name = "Nord"
description = "Inspired by the Nord colour palette"

[colors]
background = "#2e3440"
foreground = "#d8dee9"
surface = "#3b4252"
accent = "#88c0d0"
border = "#4c566a"
success = "#a3be8c"
warning = "#ebcb8b"
error = "#bf616a"
muted = "#616e88"

[tui]
roster_highlight = "#88c0d0"
status_bar_bg = "#3b4252"

[gui]
border_radius = "6px"
font_family = "Inter, sans-serif"
"##,
        )
        .unwrap();

        let config = ThemeConfig {
            name: "Nord".to_string(),
            custom_path: Some(theme_path.to_str().unwrap().to_string()),
        };
        let theme = ThemeManager::load(&config).unwrap();
        assert_eq!(theme.name, "Nord");
        assert_eq!(theme.colors.background, "#2e3440");
        assert_eq!(theme.colors.accent, "#88c0d0");

        let tui = theme.tui_overrides.unwrap();
        assert_eq!(tui.roster_highlight, Some("#88c0d0".to_string()));
        assert_eq!(tui.status_bar_bg, Some("#3b4252".to_string()));
        assert_eq!(tui.input_border, None);

        let gui = theme.gui_overrides.unwrap();
        assert_eq!(gui.border_radius, Some("6px".to_string()));
        assert_eq!(gui.font_family, Some("Inter, sans-serif".to_string()));
    }

    #[test]
    fn load_custom_theme_without_optional_sections() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("minimal.toml");
        std::fs::write(
            &theme_path,
            r##"
[meta]
name = "Minimal"

[colors]
background = "#ffffff"
foreground = "#000000"
surface = "#f0f0f0"
accent = "#0000ff"
border = "#cccccc"
success = "#00ff00"
warning = "#ffff00"
error = "#ff0000"
muted = "#999999"
"##,
        )
        .unwrap();

        let config = ThemeConfig {
            name: "Minimal".to_string(),
            custom_path: Some(theme_path.to_str().unwrap().to_string()),
        };
        let theme = ThemeManager::load(&config).unwrap();
        assert_eq!(theme.name, "Minimal");
        assert!(theme.tui_overrides.is_none());
        assert!(theme.gui_overrides.is_none());
    }

    #[test]
    fn load_custom_theme_missing_file() {
        let config = ThemeConfig {
            name: "whatever".to_string(),
            custom_path: Some("/nonexistent/path/theme.toml".to_string()),
        };
        let theme = ThemeManager::load(&config).unwrap();
        assert_eq!(theme.name, "default");
    }

    #[test]
    fn load_custom_theme_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("bad.toml");
        std::fs::write(&theme_path, "this is not valid toml {{{").unwrap();

        let config = ThemeConfig {
            name: "bad".to_string(),
            custom_path: Some(theme_path.to_str().unwrap().to_string()),
        };
        let theme = ThemeManager::load(&config).unwrap();
        assert_eq!(theme.name, "default");
    }

    #[test]
    fn load_custom_theme_invalid_color() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("badcolor.toml");
        std::fs::write(
            &theme_path,
            r##"
[meta]
name = "BadColor"

[colors]
background = "not-a-color"
foreground = "#000000"
surface = "#f0f0f0"
accent = "#0000ff"
border = "#cccccc"
success = "#00ff00"
warning = "#ffff00"
error = "#ff0000"
muted = "#999999"
"##,
        )
        .unwrap();

        let config = ThemeConfig {
            name: "BadColor".to_string(),
            custom_path: Some(theme_path.to_str().unwrap().to_string()),
        };
        let theme = ThemeManager::load(&config).unwrap();
        assert_eq!(theme.name, "default");
    }

    #[test]
    fn css_custom_properties_generation() {
        let theme = builtin_default();
        let props = theme.css_custom_properties();
        assert_eq!(props.get("--waddle-bg").unwrap(), "#ffffff");
        assert_eq!(props.get("--waddle-fg").unwrap(), "#1a1a1a");
        assert_eq!(props.get("--waddle-accent").unwrap(), "#0066cc");
        assert_eq!(props.get("--waddle-surface").unwrap(), "#f5f5f5");
        assert_eq!(props.get("--waddle-border").unwrap(), "#d4d4d4");
        assert_eq!(props.get("--waddle-success").unwrap(), "#22863a");
        assert_eq!(props.get("--waddle-warning").unwrap(), "#b08800");
        assert_eq!(props.get("--waddle-error").unwrap(), "#cb2431");
        assert_eq!(props.get("--waddle-muted").unwrap(), "#6a737d");
    }

    #[test]
    fn css_custom_properties_include_gui_overrides() {
        let mut theme = builtin_default();
        theme.gui_overrides = Some(GuiOverrides {
            border_radius: Some("8px".to_string()),
            font_family: Some("Fira Code".to_string()),
        });
        let props = theme.css_custom_properties();
        assert_eq!(props.get("--waddle-border-radius").unwrap(), "8px");
        assert_eq!(props.get("--waddle-font-family").unwrap(), "Fira Code");
    }

    #[test]
    fn available_themes_includes_builtins() {
        let manager = ThemeManager::new();
        let themes = manager.available_themes();
        assert!(themes.contains(&"default".to_string()));
        assert!(themes.contains(&"dark".to_string()));
        assert!(themes.contains(&"high-contrast".to_string()));
    }

    #[test]
    fn available_themes_includes_custom() {
        let mut manager = ThemeManager::new();
        manager.register_custom(Theme {
            name: "nord".to_string(),
            colors: builtin_default().colors,
            tui_overrides: None,
            gui_overrides: None,
            plugin_colors: HashMap::new(),
        });
        let themes = manager.available_themes();
        assert!(themes.contains(&"nord".to_string()));
        assert!(themes.contains(&"default".to_string()));
    }

    #[test]
    fn available_themes_sorted() {
        let mut manager = ThemeManager::new();
        manager.register_custom(Theme {
            name: "aardvark".to_string(),
            colors: builtin_default().colors,
            tui_overrides: None,
            gui_overrides: None,
            plugin_colors: HashMap::new(),
        });
        let themes = manager.available_themes();
        let mut sorted = themes.clone();
        sorted.sort();
        assert_eq!(themes, sorted);
    }

    #[test]
    fn get_builtin_theme() {
        let manager = ThemeManager::new();
        let theme = manager.get("dark").unwrap();
        assert_eq!(theme.name, "dark");
    }

    #[test]
    fn get_custom_theme() {
        let mut manager = ThemeManager::new();
        manager.register_custom(Theme {
            name: "custom".to_string(),
            colors: builtin_dark().colors,
            tui_overrides: None,
            gui_overrides: None,
            plugin_colors: HashMap::new(),
        });
        let theme = manager.get("custom").unwrap();
        assert_eq!(theme.name, "custom");
    }

    #[test]
    fn get_returns_none_for_unknown() {
        let manager = ThemeManager::new();
        assert!(manager.get("nonexistent").is_none());
    }

    #[test]
    fn validate_color_accepts_3_digit_hex() {
        assert!(validate_color("#fff").is_ok());
        assert!(validate_color("#abc").is_ok());
    }

    #[test]
    fn validate_color_accepts_6_digit_hex() {
        assert!(validate_color("#ffffff").is_ok());
        assert!(validate_color("#2e3440").is_ok());
    }

    #[test]
    fn validate_color_accepts_8_digit_hex() {
        assert!(validate_color("#ffffff80").is_ok());
    }

    #[test]
    fn validate_color_rejects_invalid() {
        assert!(validate_color("not-a-color").is_err());
        assert!(validate_color("#gg0000").is_err());
        assert!(validate_color("#12345").is_err());
        assert!(validate_color("").is_err());
    }

    #[test]
    fn plugin_color_returns_none_without_plugin_tokens() {
        let theme = builtin_default();
        assert!(theme.plugin_color("myplugin", "accent").is_none());
    }

    #[test]
    fn register_plugin_colors_and_read_token() {
        let mut theme = builtin_default();
        let mut colors = HashMap::new();
        colors.insert("accent".to_string(), "#ff6b6b".to_string());
        colors.insert("surface".to_string(), "#2d2d2d".to_string());

        theme.register_plugin_colors("myplugin", colors).unwrap();
        assert_eq!(
            theme.plugin_color("myplugin", "accent"),
            Some("#ff6b6b".to_string())
        );
        assert_eq!(
            theme.plugin_color("myplugin", "surface"),
            Some("#2d2d2d".to_string())
        );
    }

    #[test]
    fn register_plugin_colors_rejects_invalid_values() {
        let mut theme = builtin_default();
        let mut colors = HashMap::new();
        colors.insert("accent".to_string(), "not-a-color".to_string());

        let err = theme
            .register_plugin_colors("myplugin", colors)
            .unwrap_err();
        assert!(matches!(err, ThemeError::InvalidColor(_)));
        assert!(theme.plugin_color("myplugin", "accent").is_none());
    }

    #[test]
    fn css_custom_properties_include_plugin_colors() {
        let mut theme = builtin_default();
        let mut colors = HashMap::new();
        colors.insert("accent".to_string(), "#ff6b6b".to_string());

        theme.register_plugin_colors("myplugin", colors).unwrap();
        let props = theme.css_custom_properties();
        assert_eq!(
            props.get("--waddle-plugin-myplugin-accent").unwrap(),
            "#ff6b6b"
        );
    }

    #[test]
    fn theme_manager_default_trait() {
        let manager = ThemeManager::default();
        assert!(manager.get("default").is_some());
    }
}
