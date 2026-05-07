use std::path::PathBuf;
use std::time::Duration;

use rusqlite::OpenFlags;

/// How the DB pages are stored.
#[derive(Debug, Clone)]
pub enum SqliteBacking {
    /// On-disk WAL file. The path to the `.db`.
    Disk(PathBuf),
    /// Shared-cache in-memory DB, snapshotted to `snapshot_to` every
    /// `flush_interval`. `name` scopes the shared in-memory DB within the
    /// process - two `Memory` stores with different names are independent.
    Memory {
        name: String,
        snapshot_to: PathBuf,
        flush_interval: Duration,
    },
}

impl SqliteBacking {
    pub(crate) fn connection_uri(&self) -> String {
        match self {
            Self::Disk(p) => p.to_string_lossy().into_owned(),
            Self::Memory { name, .. } => {
                format!("file:{name}?mode=memory&cache=shared")
            }
        }
    }

    pub(crate) fn open_flags(&self) -> OpenFlags {
        let mut f = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
        // NO_MUTEX is fine here: rusqlite wraps Connection in its own Mutex
        // for Send bounds. For shared-cache memory we need URI parsing.
        if matches!(self, Self::Memory { .. }) {
            f |= OpenFlags::SQLITE_OPEN_URI;
        }
        f
    }

    pub(crate) fn on_disk(&self) -> bool {
        matches!(self, Self::Disk(_))
    }
}
