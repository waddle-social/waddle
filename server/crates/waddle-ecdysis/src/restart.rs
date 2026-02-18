//! Process re-execution with file descriptor passing.
//!
//! Implements the Ecdysis restart: clear CLOEXEC on listening sockets,
//! set LISTEN_FDS/LISTEN_FD_NAMES env vars, then exec the current binary.

use std::os::unix::io::AsRawFd;
use tokio::net::TcpListener;
use tracing::{error, info};

/// Error returned when restart fails.
/// Contains the listeners so the caller can resume serving.
#[derive(Debug)]
pub struct RestartError {
    /// The listeners that were going to be passed to the new process.
    pub listeners: Vec<(String, TcpListener)>,
    /// The underlying error.
    pub error: std::io::Error,
}

impl std::fmt::Display for RestartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Restart failed: {}", self.error)
    }
}

impl std::error::Error for RestartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Restart the current process, passing the given listeners to the new process.
///
/// 1. Converts listeners to std, gets raw fds
/// 2. Dups each fd to contiguous range fd 3..N (safe against clobber)
/// 3. Clears CLOEXEC on each target fd
/// 4. Sets LISTEN_FDS and LISTEN_FD_NAMES via execve environment
/// 5. Execs the current binary
///
/// On exec failure, panics. A half-exec'd state is not recoverable safely
/// in a multi-threaded async runtime — the env is poisoned and fds are
/// in an indeterminate state. Crash and let systemd restart us clean.
///
/// On success, this function does not return (exec replaces the process).
pub fn restart(listeners: Vec<(String, TcpListener)>) -> ! {
    let exe = std::env::current_exe().expect("Failed to determine current executable path");

    info!(exe = %exe.display(), listener_count = listeners.len(), "Preparing restart");

    // Convert tokio listeners to std
    let std_listeners: Vec<(String, std::net::TcpListener)> = listeners
        .into_iter()
        .map(|(name, listener)| {
            let std = listener
                .into_std()
                .expect("Failed to convert tokio listener to std");
            (name, std)
        })
        .collect();

    let names: Vec<String> = std_listeners.iter().map(|(n, _)| n.clone()).collect();

    // Phase 1: Dup all source fds to high fds first to avoid clobbering.
    // If a source fd is at 4 and we need to dup another to 4, we'd destroy it.
    // Solution: dup all sources to temporary high fds, then dup2 to targets.
    let mut temp_fds: Vec<(String, i32)> = Vec::with_capacity(std_listeners.len());
    for (name, listener) in &std_listeners {
        let source_fd = listener.as_raw_fd();
        // F_DUPFD_CLOEXEC gives us a new fd >= the hint, with CLOEXEC set.
        // We use a high base (100) to stay out of the 3..N target range.
        let temp_fd = unsafe { libc::fcntl(source_fd, libc::F_DUPFD_CLOEXEC, 100) };
        assert!(
            temp_fd >= 0,
            "fcntl F_DUPFD_CLOEXEC failed for listener '{}' (fd {}): {}",
            name,
            source_fd,
            std::io::Error::last_os_error()
        );
        temp_fds.push((name.clone(), temp_fd));
    }

    // Drop the original std_listeners — we have the fds duped to temp slots
    drop(std_listeners);

    // Phase 2: Dup2 from temp fds to target range 3..N
    let mut target_fds: Vec<i32> = Vec::with_capacity(temp_fds.len());
    for (i, (name, temp_fd)) in temp_fds.iter().enumerate() {
        let target_fd = 3 + i as i32;
        let result = unsafe { libc::dup2(*temp_fd, target_fd) };
        assert!(
            result >= 0,
            "dup2({} -> {}) failed for listener '{}': {}",
            temp_fd,
            target_fd,
            name,
            std::io::Error::last_os_error()
        );

        // Clear CLOEXEC on the target fd so it survives exec
        let flags = unsafe { libc::fcntl(target_fd, libc::F_GETFD) };
        assert!(flags >= 0, "fcntl F_GETFD failed on fd {}", target_fd);
        let result = unsafe { libc::fcntl(target_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
        assert!(result >= 0, "fcntl F_SETFD failed on fd {}", target_fd);

        target_fds.push(target_fd);
    }

    // Close temp fds (they have CLOEXEC so exec would close them, but be explicit)
    for (_, temp_fd) in &temp_fds {
        unsafe { libc::close(*temp_fd) };
    }

    info!(
        fds = ?target_fds,
        names = ?names,
        exe = %exe.display(),
        "Executing new process"
    );

    // Build the new environment with LISTEN_FDS and LISTEN_FD_NAMES.
    // Use execve with explicit env to avoid set_var UB in multi-threaded context.
    let args: Vec<String> = std::env::args().collect();
    exec_with_env(
        &exe,
        &args,
        &[
            ("LISTEN_FDS", &temp_fds.len().to_string()),
            ("LISTEN_FD_NAMES", &names.join(":")),
        ],
    );
}

/// Execute a new process image with additional environment variables.
/// Does not return on success. Panics on failure.
fn exec_with_env(exe: &std::path::Path, args: &[String], extra_env: &[(&str, &str)]) -> ! {
    use std::ffi::CString;

    let c_exe =
        CString::new(exe.to_string_lossy().as_bytes()).expect("Executable path contains null byte");

    let c_args: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_bytes()).expect("Arg contains null byte"))
        .collect();

    // Build environment: inherit current env + add our extras
    let mut env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
    for (k, v) in extra_env {
        env_map.insert(k.to_string(), v.to_string());
    }

    let c_env: Vec<CString> = env_map
        .iter()
        .map(|(k, v)| CString::new(format!("{}={}", k, v)).expect("Env var contains null byte"))
        .collect();

    let c_arg_ptrs: Vec<*const libc::c_char> = c_args
        .iter()
        .map(|a| a.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let c_env_ptrs: Vec<*const libc::c_char> = c_env
        .iter()
        .map(|e| e.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    unsafe {
        libc::execve(c_exe.as_ptr(), c_arg_ptrs.as_ptr(), c_env_ptrs.as_ptr());
    }

    // execve only returns on error
    let err = std::io::Error::last_os_error();
    panic!("execve failed: {} (exe: {})", err, exe.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CLOEXEC flag management works correctly.
    #[test]
    fn test_cloexec_management() {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);

        // Set CLOEXEC
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };

        // Verify set
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);

        // Clear CLOEXEC
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };

        // Verify cleared
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(flags & libc::FD_CLOEXEC, 0);

        unsafe { libc::close(fd) };
    }

    /// F_DUPFD_CLOEXEC produces a high fd with CLOEXEC set.
    #[test]
    fn test_dupfd_cloexec_to_high_range() {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);

        let high_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 100) };
        assert!(high_fd >= 100);

        // Verify CLOEXEC is set on the duped fd
        let flags = unsafe { libc::fcntl(high_fd, libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);

        unsafe {
            libc::close(fd);
            libc::close(high_fd);
        }
    }
}
