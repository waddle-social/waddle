//! App preferences, kept in a small typed JSON file in the XDG config
//! directory (`$XDG_CONFIG_HOME/waddle/settings.json`).
//!
//! A `gio::Settings` schema would need `glib-compile-schemas` in the dev
//! shell, CI and every package; a file this small does not earn that. The
//! file holds no secrets (sessions live in the Secret Service), is written
//! atomically, and a missing or damaged file means the defaults.

use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Light or dark, or whatever the desktop uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// Which end of the timeline is the newest message. The web calls this the
/// scroll direction; Apple calls it Social mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageOrder {
    /// Newest at the bottom, composer at the bottom.
    #[default]
    Chat,
    /// Newest first, composer on top, older history loads at the bottom.
    Social,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub message_order: MessageOrder,
    /// Desktop notifications at all.
    pub notifications: bool,
    /// Rooms with no mode of their own notify only for mentions. Direct
    /// messages always notify, and a mode set on a room beats this.
    pub rooms_mentions_only: bool,
    /// Author names in their XEP-0392 color. XEP-0392 says implementations
    /// SHOULD let the user turn colorization off.
    pub colorize_names: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            message_order: MessageOrder::default(),
            notifications: true,
            rooms_mentions_only: true,
            colorize_names: true,
        }
    }
}

impl Settings {
    /// Read `path`. A missing file is the defaults; a damaged one is too,
    /// and is left alone until the next save replaces it.
    pub fn load_from(path: &Path) -> Self {
        match fs::read(path) {
            Ok(bytes) => Self::parse(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, path = %path.display(), "settings are unreadable; using defaults");
                Self::default()
            }),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(%error, path = %path.display(), "could not read settings");
                }
                Self::default()
            }
        }
    }

    fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Write `path` through a temporary file and a rename, so a crash
    /// never leaves half a file.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::other("settings path has no directory"))?;
        fs::create_dir_all(dir)?;
        let mut json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        json.push(b'\n');
        let temporary = path.with_extension("json.tmp");
        let written = fs::File::create(&temporary).and_then(|mut file| {
            file.write_all(&json)?;
            file.sync_all()
        });
        if let Err(error) = written.and_then(|()| fs::rename(&temporary, path)) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(())
    }
}

fn config_path() -> PathBuf {
    gtk::glib::user_config_dir()
        .join("waddle")
        .join("settings.json")
}

thread_local! {
    // Read from the GTK main thread only, like the widgets it configures.
    static CURRENT: RefCell<Option<Settings>> = const { RefCell::new(None) };
}

/// The settings in force, loaded from disk on first use.
pub fn get() -> Settings {
    CURRENT.with(|current| {
        current
            .borrow_mut()
            .get_or_insert_with(|| Settings::load_from(&config_path()))
            .clone()
    })
}

/// Change the settings and save them. The change holds for this run even
/// if the save fails; the error says why it will not survive a restart.
pub fn update(change: impl FnOnce(&mut Settings)) -> std::io::Result<Settings> {
    let mut next = get();
    change(&mut next);
    CURRENT.with(|current| *current.borrow_mut() = Some(next.clone()));
    next.save_to(&config_path())?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("waddle-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    #[test]
    fn defaults_are_system_chat_and_mentions() {
        let defaults = Settings::default();
        assert_eq!(defaults.appearance, Appearance::System);
        assert_eq!(defaults.message_order, MessageOrder::Chat);
        assert!(defaults.notifications);
        assert!(defaults.rooms_mentions_only);
    }

    #[test]
    fn missing_fields_take_defaults_and_unknown_ones_are_ignored() {
        let parsed = Settings::parse(br#"{"appearance":"dark","future":1}"#).expect("parses");
        assert_eq!(parsed.appearance, Appearance::Dark);
        assert_eq!(parsed.message_order, MessageOrder::Chat);
        assert!(parsed.notifications);
    }

    #[test]
    fn values_use_stable_names() {
        let json = serde_json::to_string(&Settings {
            appearance: Appearance::Light,
            message_order: MessageOrder::Social,
            notifications: false,
            rooms_mentions_only: false,
        })
        .expect("serializes");
        assert_eq!(
            json,
            r#"{"appearance":"light","message_order":"social","notifications":false,"rooms_mentions_only":false}"#
        );
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let dir = scratch();
        assert_eq!(
            Settings::load_from(&dir.join("settings.json")),
            Settings::default()
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_corrupt_file_is_the_defaults() {
        let dir = scratch();
        let path = dir.join("settings.json");
        for contents in ["", "not json", r#"{"appearance":"sepia"}"#, "[1,2]"] {
            fs::write(&path, contents).expect("write");
            assert_eq!(
                Settings::load_from(&path),
                Settings::default(),
                "{contents}"
            );
        }
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn saving_round_trips_and_leaves_no_temporary_file() {
        let dir = scratch();
        let path = dir.join("nested").join("settings.json");
        let settings = Settings {
            appearance: Appearance::Dark,
            message_order: MessageOrder::Social,
            notifications: false,
            rooms_mentions_only: false,
            colorize_names: false,
        };
        settings.save_to(&path).expect("saves");
        assert_eq!(Settings::load_from(&path), settings);
        let names: Vec<_> = fs::read_dir(path.parent().expect("dir"))
            .expect("lists")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["settings.json"]);
        // A second save replaces the first.
        Settings::default().save_to(&path).expect("saves again");
        assert_eq!(Settings::load_from(&path), Settings::default());
        fs::remove_dir_all(dir).ok();
    }
}
