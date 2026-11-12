//! Output files of the operator subcommands.

use std::io::Write;
use std::path::Path;

use tempfile::NamedTempFile;

use super::error::OwnerCliError;

/// Write `contents` to `path` so that a reader sees the previous file or the
/// complete new one, never a partial write.
///
/// The contents go to a temporary file in the same directory, which then
/// replaces `path` by rename. An existing regular file is replaced, so a job
/// that runs again produces the same file. A `path` that is a symbolic link
/// is refused: the subcommand never writes through a link to a file it was
/// not pointed at.
pub(super) fn write_output(path: &Path, contents: &str) -> Result<(), OwnerCliError> {
    let failed = |source| OwnerCliError::Write {
        path: path.to_owned(),
        source,
    };
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(OwnerCliError::OutputIsSymlink {
            path: path.to_owned(),
        });
    }
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut file = NamedTempFile::new_in(directory).map_err(failed)?;
    file.write_all(contents.as_bytes()).map_err(failed)?;
    file.as_file().sync_all().map_err(failed)?;
    file.persist(path).map_err(|error| failed(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_output;
    use crate::owner_cli::error::OwnerCliError;

    #[test]
    fn replaces_a_regular_file_and_refuses_a_symbolic_link() {
        let directory = tempfile::tempdir().expect("output directory");
        let target = directory.path().join("handoff.json");
        write_output(&target, "first").expect("first write");
        write_output(&target, "second").expect("repeated write");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "second");
        assert_eq!(
            std::fs::read_dir(directory.path()).expect("list").count(),
            1,
            "no temporary file is left behind"
        );

        let link = directory.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).expect("symbolic link");
        assert!(matches!(
            write_output(&link, "third"),
            Err(OwnerCliError::OutputIsSymlink { .. })
        ));
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "second");
    }
}
