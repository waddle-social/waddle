//! Listener inheritance from file descriptors.
//!
//! Checks `LISTEN_FDS` and `LISTEN_FD_NAMES` environment variables
//! (compatible with systemd socket activation / Ecdysis fd passing).
//! Fails hard if the env says fds exist but they're invalid.

use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use tokio::net::TcpListener;
use tracing::info;

/// A set of named listeners inherited from the parent process via fd passing.
pub struct ListenerSet {
    listeners: Vec<(String, TcpListener)>,
}

/// Owned inherited sockets captured before runtime threads are started.
///
/// Convert with [`Self::into_listener_set`] after entering the Tokio runtime.
pub struct InheritedListeners {
    listeners: Vec<(String, std::net::TcpListener)>,
}

impl InheritedListeners {
    /// Capture a listener set from descriptors passed exclusively by the launcher.
    ///
    /// Reads `LISTEN_FDS` and `LISTEN_FD_NAMES` from the environment.
    /// Returns `None` if `LISTEN_FDS` is absent or "0" (cold start).
    ///
    /// The environment is retained; adopted descriptors are close-on-exec.
    ///
    /// # Safety
    ///
    /// When `LISTEN_FDS` is nonzero, call once in the process's synchronous
    /// startup prologue, before creating threads or opening other descriptors.
    /// The launcher must have transferred exclusive ownership of descriptors
    /// `3..3 + LISTEN_FDS` to this process. No Rust object or other code may own,
    /// close, or reuse them. A socket validation check alone cannot prove that
    /// ownership requirement.
    ///
    /// # Panics
    ///
    /// Panics if inherited descriptor names/counts are inconsistent, a descriptor
    /// is invalid or not a socket, or configuring a descriptor fails.
    pub unsafe fn from_env() -> Option<Self> {
        let listen_fds: usize = std::env::var("LISTEN_FDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        if listen_fds == 0 {
            return None;
        }

        let names_raw = std::env::var("LISTEN_FD_NAMES")
            .expect("LISTEN_FDS is set but LISTEN_FD_NAMES is missing — parent process bug");

        let names: Vec<&str> = names_raw.split(':').collect();
        assert_eq!(
            names.len(),
            listen_fds,
            "LISTEN_FD_NAMES has {} entries but LISTEN_FDS says {} — parent process bug",
            names.len(),
            listen_fds,
        );

        let count = i32::try_from(listen_fds).expect("LISTEN_FDS exceeds descriptor range");
        count
            .checked_add(3)
            .expect("LISTEN_FDS exceeds descriptor range");
        let mut listeners = Vec::with_capacity(listen_fds);

        for (i, name) in names.iter().enumerate().take(listen_fds) {
            let fd = 3 + i as i32;

            assert!(
                validate_fd(fd),
                "Inherited fd {} (name: {}) is not a valid socket — parent process bug",
                fd,
                name,
            );

            // SAFETY: The caller guarantees these valid launcher-transferred
            // descriptors have no other owner, and that capture runs only once.
            // Each distinct descriptor is immediately placed under RAII ownership.
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            set_close_on_exec(owned.as_fd())
                .expect("Failed to mark inherited listener close-on-exec");
            let listener = std::net::TcpListener::from(owned);
            listener
                .set_nonblocking(true)
                .expect("Failed to set inherited listener to non-blocking");
            listeners.push(((*name).to_string(), listener));
        }

        Some(Self { listeners })
    }

    /// Register captured sockets with the currently entered Tokio runtime.
    ///
    /// # Panics
    ///
    /// Panics outside a Tokio runtime or if registering an inherited socket fails.
    pub fn into_listener_set(self) -> ListenerSet {
        let listeners = self
            .listeners
            .into_iter()
            .map(|(name, listener)| {
                let fd = listener.as_raw_fd();
                let listener = TcpListener::from_std(listener)
                    .expect("Failed to convert inherited fd to tokio listener");
                let addr = listener.local_addr().ok();
                info!(fd, name, addr = ?addr, "Inherited listener from launcher");
                (name, listener)
            })
            .collect();
        ListenerSet { listeners }
    }
}

impl ListenerSet {
    /// Take a listener by name, removing it from the set.
    ///
    /// # Panics
    ///
    /// Panics if no listener with the given name exists. The parent process
    /// must pass all expected listeners.
    pub fn take(&mut self, name: &str) -> TcpListener {
        let pos = self
            .listeners
            .iter()
            .position(|(n, _)| n == name)
            .unwrap_or_else(|| {
                let available: Vec<&str> = self.listeners.iter().map(|(n, _)| n.as_str()).collect();
                panic!(
                    "Expected inherited listener '{}' but only have {:?} — parent process bug",
                    name, available
                );
            });
        self.listeners.remove(pos).1
    }

    /// Assert that all listeners have been consumed.
    ///
    /// # Panics
    ///
    /// Panics if any listeners remain. This catches mismatches between
    /// what the parent passed and what the child expected.
    pub fn assert_empty(self) {
        if !self.listeners.is_empty() {
            let remaining: Vec<&str> = self.listeners.iter().map(|(n, _)| n.as_str()).collect();
            panic!(
                "Unconsumed inherited listeners: {:?} — listener name mismatch between old and new binary",
                remaining
            );
        }
    }
}

/// Validate an unowned descriptor number before adoption. The standard library
/// has no socket-kind query that accepts a potentially invalid raw descriptor.
fn validate_fd(fd: RawFd) -> bool {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat only writes through the properly aligned, writable stat
    // pointer. An invalid descriptor is reported as an error without reading it.
    let result = unsafe { libc::fstat(fd, stat.as_mut_ptr()) };
    if result != 0 {
        return false;
    }
    // SAFETY: A successful fstat initializes the stat structure's fields.
    let stat = unsafe { stat.assume_init() };
    (stat.st_mode & libc::S_IFMT) == libc::S_IFSOCK
}

/// Set close-on-exec without taking or invalidating the borrowed descriptor.
fn set_close_on_exec(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: fd remains valid for this borrow; F_GETFD does not access pointers
    // or change descriptor ownership. The standard library has no equivalent.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The same valid borrowed descriptor is used with integer flags;
    // F_SETFD changes only descriptor flags and does not transfer ownership.
    let result = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    /// Cold start: no env vars → from_env returns None.
    #[test]
    fn test_cold_start_returns_none() {
        const MARKER: &str = "WADDLE_LISTENER_COLD_START_TEST";
        if std::env::var_os(MARKER).is_some() {
            assert!(std::env::var_os("LISTEN_FDS").is_none());
            // SAFETY: The subprocess starts without inheritance metadata, so
            // this call cannot adopt any raw descriptors.
            assert!(unsafe { InheritedListeners::from_env() }.is_none());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "listener::tests::test_cold_start_returns_none"])
            .env(MARKER, "1")
            .env_remove("LISTEN_FDS")
            .env_remove("LISTEN_FD_NAMES")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    /// fd inheritance round-trip: bind → dup → adopt → connect.
    #[tokio::test]
    async fn test_fd_inheritance_round_trip() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let fd = listener.as_raw_fd();

        let duplicate = listener.as_fd().try_clone_to_owned().unwrap();
        assert!(validate_fd(duplicate.as_raw_fd()));
        assert!(validate_fd(fd));

        let std_listener = std::net::TcpListener::from(duplicate);
        std_listener.set_nonblocking(true).unwrap();
        let tokio_listener = TcpListener::from_std(std_listener).unwrap();

        assert_eq!(tokio_listener.local_addr().unwrap(), addr);

        let stream = tokio::net::TcpStream::connect(addr).await;
        assert!(stream.is_ok());

        drop(listener);
    }

    /// Invalid fd is detected.
    #[test]
    fn test_invalid_fd_detection() {
        assert!(!validate_fd(9999));
        assert!(!validate_fd(-1));
    }

    /// take() on missing name → panic.
    #[tokio::test]
    #[should_panic(expected = "Expected inherited listener 'missing'")]
    async fn test_take_missing_panics() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let tokio_listener = TcpListener::from_std(listener).unwrap();
        let mut set = ListenerSet {
            listeners: vec![("http".to_string(), tokio_listener)],
        };
        set.take("missing");
    }

    /// assert_empty with remaining listeners → panic.
    #[tokio::test]
    #[should_panic(expected = "Unconsumed inherited listeners")]
    async fn test_assert_empty_panics() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let tokio_listener = TcpListener::from_std(listener).unwrap();
        let set = ListenerSet {
            listeners: vec![("http".to_string(), tokio_listener)],
        };
        set.assert_empty();
    }
}
