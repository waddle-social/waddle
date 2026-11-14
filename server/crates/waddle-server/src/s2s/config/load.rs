//! Whether S2S is enabled, and bounded reading of the files a snapshot
//! names.
//!
//! S2S is off unless `WADDLE_S2S_ENABLED` is `true`. Only then is
//! `WADDLE_S2S_CONFIG_FILE` read; while S2S is off, no S2S file is opened,
//! no listener is bound and no peer is dialled.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use tokio::fs::File;
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

use super::error::{ConfigError, ConfigSection};
use super::limits::{MAX_CONFIG_FILE_BYTES, MAX_MATERIAL_FILE_BYTES, MAX_TOTAL_MATERIAL_BYTES};

/// The variable that enables S2S.
pub const ENABLED_VARIABLE: &str = "WADDLE_S2S_ENABLED";

/// The variable naming the S2S configuration file.
pub const CONFIG_FILE_VARIABLE: &str = "WADDLE_S2S_CONFIG_FILE";

/// Whether this process runs S2S, as the environment says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S2sActivation {
    /// S2S is off: the default.
    Disabled,
    /// S2S is on, configured by this file.
    Enabled(PathBuf),
}

impl S2sActivation {
    /// Read the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_os_values(
            std::env::var_os(ENABLED_VARIABLE).as_deref(),
            std::env::var_os(CONFIG_FILE_VARIABLE).map(PathBuf::from),
        )
    }

    /// Decide from the variables as the environment holds them. A flag
    /// that is not Unicode is neither `true` nor `false`, so it fails like
    /// any other value instead of reading as unset.
    pub fn from_os_values(
        enabled: Option<&OsStr>,
        file: Option<PathBuf>,
    ) -> Result<Self, ConfigError> {
        let enabled = enabled
            .map(|value| value.to_str().ok_or(ConfigError::InvalidEnabledFlag))
            .transpose()?;
        Self::from_values(enabled, file)
    }

    /// Decide from the two variables' values. An unset or `false` flag
    /// disables S2S whatever the file variable says; `true` requires an
    /// absolute file path.
    pub fn from_values(enabled: Option<&str>, file: Option<PathBuf>) -> Result<Self, ConfigError> {
        match enabled {
            None | Some("false") => Ok(Self::Disabled),
            Some("true") => {
                let file = file.ok_or(ConfigError::MissingConfigFile)?;
                if !file.is_absolute() {
                    return Err(ConfigError::RelativeConfigFile);
                }
                Ok(Self::Enabled(file))
            }
            Some(_) => Err(ConfigError::InvalidEnabledFlag),
        }
    }
}

/// Reads configuration and material files within the size limits, counting
/// every byte read for one snapshot against the total limit.
#[derive(Debug, Default)]
pub(super) struct MaterialReader {
    total: u64,
}

impl MaterialReader {
    /// Read the configuration file as UTF-8.
    pub(super) async fn read_config(&mut self, path: &Path) -> Result<String, ConfigError> {
        let bytes = self
            .read_bounded(path, MAX_CONFIG_FILE_BYTES)
            .await
            .map_err(|error| match error {
                ReadError::Unreadable => ConfigError::ConfigFileUnreadable,
                ReadError::TooLarge => ConfigError::ConfigFileTooLarge,
                ReadError::TotalTooLarge => ConfigError::TotalTooLarge,
            })?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ConfigError::ConfigFileUnreadable)
    }

    /// Read a certificate, key or CRL file named by `section`. The buffer
    /// is wiped when it is dropped, because it may hold a private key.
    pub(super) async fn read(
        &mut self,
        path: &Path,
        section: ConfigSection,
    ) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
        if !path.is_absolute() {
            return Err(ConfigError::RelativePath(section));
        }
        self.read_bounded(path, MAX_MATERIAL_FILE_BYTES)
            .await
            .map_err(|error| match error {
                ReadError::Unreadable => ConfigError::MaterialUnreadable(section),
                ReadError::TooLarge => ConfigError::MaterialTooLarge(section),
                ReadError::TotalTooLarge => ConfigError::TotalTooLarge,
            })
    }

    /// Read at most `limit` bytes, failing when the file holds more. The
    /// read itself is bounded, so a file that grows while it is read cannot
    /// exceed the limit.
    async fn read_bounded(
        &mut self,
        path: &Path,
        limit: u64,
    ) -> Result<Zeroizing<Vec<u8>>, ReadError> {
        let file = File::open(path).await.map_err(|_| ReadError::Unreadable)?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ReadError::Unreadable)?;
        let read = u64::try_from(bytes.len()).map_err(|_| ReadError::TooLarge)?;
        if read > limit {
            return Err(ReadError::TooLarge);
        }
        self.total = self.total.saturating_add(read);
        if self.total > MAX_TOTAL_MATERIAL_BYTES {
            return Err(ReadError::TotalTooLarge);
        }
        Ok(bytes)
    }
}

enum ReadError {
    Unreadable,
    TooLarge,
    TotalTooLarge,
}
