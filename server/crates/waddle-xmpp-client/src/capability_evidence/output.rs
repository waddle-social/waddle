use std::io::Write;
use std::path::{Path, PathBuf};

use super::model::CapabilityEvidenceError;

pub(super) fn write_artifact(output: &Path, contents: &str) -> Result<(), CapabilityEvidenceError> {
    if output.exists() {
        return Err(CapabilityEvidenceError::OutputExists);
    }
    if output.is_symlink() || output.is_dir() {
        return Err(CapabilityEvidenceError::InvalidArgument(
            "output must be a regular JSON file",
        ));
    }
    let parent = output
        .parent()
        .ok_or(CapabilityEvidenceError::InvalidArgument(
            "output must have a parent directory",
        ))?;
    reject_symlinked_path(parent)?;
    std::fs::create_dir_all(parent)?;
    reject_symlinked_path(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capability-evidence"),
        uuid::Uuid::new_v4()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    let linked = std::fs::hard_link(&temporary, output);
    let _ = std::fs::remove_file(&temporary);
    match linked {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(CapabilityEvidenceError::OutputExists)
        }
        Err(error) => Err(error.into()),
    }
}

fn reject_symlinked_path(path: &Path) -> Result<(), CapabilityEvidenceError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "output path must not traverse parent directories",
            ));
        }
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CapabilityEvidenceError::InvalidArgument(
                    "output path must not contain symlinked parents",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
