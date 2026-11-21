//! Whether any process opens or reads a set of files, observed with Linux
//! inotify. The S2S-off row uses it to show that a deployment with S2S
//! disabled opens neither its S2S configuration nor the credentials that
//! configuration names. Inotify does not say which process opened a file,
//! so the watched files must be ones no other process touches.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// The fixed part of one `struct inotify_event`: `wd`, `mask`, `cookie`
/// and `len`, each 32 bits.
const EVENT_HEADER: usize = 16;

pub struct OpenWatch {
    events: File,
    watches: BTreeMap<i32, PathBuf>,
}

impl OpenWatch {
    /// Watch `paths` for opens and reads from now on.
    pub fn new(paths: &[PathBuf]) -> Result<Self> {
        // SAFETY: `inotify_init1` takes no pointers.
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error()).context("inotify_init1");
        }
        // SAFETY: `raw` is a descriptor `inotify_init1` just returned and
        // nothing else owns.
        let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut watches = BTreeMap::new();
        for path in paths {
            let name = CString::new(path.as_os_str().as_bytes())
                .with_context(|| format!("a path without NUL: {}", path.display()))?;
            // SAFETY: `name` is a NUL-terminated string that outlives the
            // call, and the descriptor is open.
            let watch = unsafe {
                libc::inotify_add_watch(
                    descriptor.as_raw_fd(),
                    name.as_ptr(),
                    libc::IN_OPEN | libc::IN_ACCESS,
                )
            };
            if watch < 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("watch {}", path.display()));
            }
            watches.insert(watch, path.clone());
        }
        Ok(Self {
            events: File::from(descriptor),
            watches,
        })
    }

    /// The file names of the watched files opened or read since the watch
    /// began (or since the last call).
    pub fn touched(&mut self) -> Result<Vec<String>> {
        let mut buffer = vec![0u8; 64 * 1024];
        let mut touched = BTreeSet::new();
        loop {
            let read = match self.events.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(error).context("read inotify events"),
            };
            let events = buffer.get(..read).unwrap_or_default();
            let mut offset = 0;
            while let Some(header) = events.get(offset..offset + EVENT_HEADER) {
                let word = |at: usize| -> [u8; 4] {
                    let mut bytes = [0u8; 4];
                    bytes.copy_from_slice(header.get(at..at + 4).unwrap_or(&[0; 4]));
                    bytes
                };
                let watch = i32::from_ne_bytes(word(0));
                let name_length = usize::try_from(u32::from_ne_bytes(word(12)))?;
                if let Some(path) = self.watches.get(&watch) {
                    touched.insert(
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    );
                }
                offset += EVENT_HEADER + name_length;
            }
        }
        Ok(touched.into_iter().collect())
    }
}
