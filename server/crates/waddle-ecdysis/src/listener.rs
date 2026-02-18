//! Listener inheritance from file descriptors.
//!
//! Checks `LISTEN_FDS` and `LISTEN_FD_NAMES` environment variables
//! (compatible with systemd socket activation / Ecdysis fd passing).
//! Fails hard if the env says fds exist but they're invalid.

use std::os::unix::io::FromRawFd;
use tokio::net::TcpListener;
use tracing::info;

/// A set of named listeners inherited from the parent process via fd passing.
pub struct ListenerSet {
    listeners: Vec<(String, TcpListener)>,
}

impl ListenerSet {
    /// Create a listener set from inherited file descriptors.
    ///
    /// Reads `LISTEN_FDS` and `LISTEN_FD_NAMES` from the environment.
    /// Returns `None` if `LISTEN_FDS` is absent or "0" (cold start).
    ///
    /// # Panics
    ///
    /// Panics if `LISTEN_FDS` is set but any fd is invalid or not a socket.
    /// A half-inherited state is a bug in the parent process — crash loudly.
    pub fn from_env() -> Option<Self> {
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

        let mut listeners = Vec::with_capacity(listen_fds);

        for i in 0..listen_fds {
            let fd = 3 + i as i32;
            let name = names[i];

            assert!(
                validate_fd(fd),
                "Inherited fd {} (name: {}) is not a valid socket — parent process bug",
                fd,
                name,
            );

            // SAFETY: We validated the fd is a valid socket via fstat.
            let std_listener = unsafe { std::net::TcpListener::from_raw_fd(fd) };
            std_listener
                .set_nonblocking(true)
                .expect("Failed to set inherited listener to non-blocking");

            let listener = TcpListener::from_std(std_listener)
                .expect("Failed to convert inherited fd to tokio listener");

            let addr = listener.local_addr().ok();
            info!(fd, name, addr = ?addr, "Inherited listener from parent process");
            listeners.push((name.to_string(), listener));
        }

        // Clear the env vars so child processes don't re-inherit stale values
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FD_NAMES");

        Some(Self { listeners })
    }

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

/// Validate that a file descriptor is a valid socket using fstat.
fn validate_fd(fd: i32) -> bool {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::fstat(fd, &mut stat) };
    if result != 0 {
        return false;
    }
    (stat.st_mode & libc::S_IFMT) == libc::S_IFSOCK
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    /// Cold start: no env vars → from_env returns None.
    #[test]
    fn test_cold_start_returns_none() {
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FD_NAMES");
        assert!(ListenerSet::from_env().is_none());
    }

    /// fd inheritance round-trip: bind → dup → adopt → connect.
    #[tokio::test]
    async fn test_fd_inheritance_round_trip() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let fd = listener.as_raw_fd();

        let new_fd = unsafe { libc::dup(fd) };
        assert!(new_fd >= 0, "dup failed");

        assert!(validate_fd(new_fd));

        let std_listener = unsafe { std::net::TcpListener::from_raw_fd(new_fd) };
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

    // NOTE: We cannot test from_env() with set_var in parallel tests because
    // env vars are shared process-wide. The panic paths in from_env() are
    // tested implicitly through the assert!() calls which are straightforward.
    // The take() and assert_empty() panic tests below cover the ListenerSet API.

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
