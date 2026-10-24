//! Exercise the launcher's descriptor handoff before any runtime or test worker
//! threads exist. The listing response lets nextest discover this native test.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use waddle_ecdysis::InheritedListeners;

const TEST_NAME: &str = "inherited_sockets_are_adopted_before_runtime_and_closed";
const CHILD_MODE: &str = "WADDLE_LISTENER_INHERITANCE_TEST_MODE";

fn main() {
    if let Some(mode) = std::env::var_os(CHILD_MODE) {
        // SAFETY: This is synchronous child startup after exec, before threads,
        // a runtime, or other descriptor owners are created.
        // The fixture launcher transferred exclusive ownership of socket fd 3.
        let inherited = unsafe { InheritedListeners::from_env() }.expect("inherited socket");
        assert_eq!(std::env::var("LISTEN_FDS").as_deref(), Ok("1"));
        assert_eq!(std::env::var("LISTEN_FD_NAMES").as_deref(), Ok("http"));

        if mode == "drop" {
            drop(inherited);
            // SAFETY: F_GETFD takes an integer, not a pointer. A closed fd is
            // deliberately queried to observe EBADF; no ownership is created.
            assert_eq!(unsafe { libc::fcntl(3, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            return;
        }
        assert_eq!(mode, "accept");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime")
            .block_on(async move {
                let mut listeners = inherited.into_listener_set();
                let listener = listeners.take("http");
                listeners.assert_empty();
                assert_close_on_exec(listener.as_fd());
                tokio::time::timeout(Duration::from_secs(5), async move {
                    let (mut stream, _) = listener.accept().await.expect("accept inherited socket");
                    let mut request = [0; 4];
                    stream.read_exact(&mut request).await.expect("read request");
                    assert_eq!(&request, b"ping");
                    stream.write_all(b"pong").await.expect("write response");
                })
                .await
                .expect("inherited listener exchange timed out");
            });
        return;
    }

    // Follow nextest's custom harness listing contract, including an empty
    // ignored-test list: https://nexte.st/docs/design/custom-test-harnesses/
    // Execution uses this same synchronous main, without libtest workers.
    let arguments: Vec<_> = std::env::args().collect();
    let ignored = arguments.iter().any(|argument| argument == "--ignored");
    if arguments.iter().any(|argument| argument == "--list") {
        if !ignored {
            println!("{TEST_NAME}: test");
        }
        return;
    }
    if ignored {
        return;
    }

    let (child, _) = launch("drop");
    assert_child_succeeded(child);

    let (child, address) = launch("accept");
    let exchange = (|| -> std::io::Result<()> {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(b"ping")?;
        let mut response = [0; 4];
        stream.read_exact(&mut response)?;
        assert_eq!(&response, b"pong");
        Ok(())
    })();
    assert_child_succeeded(child);
    exchange.expect("exchange over inherited socket");
}

fn assert_close_on_exec(fd: BorrowedFd<'_>) {
    // SAFETY: The borrow keeps fd valid; F_GETFD reads only integer descriptor
    // flags and neither accesses pointers nor changes descriptor ownership.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    assert_ne!(flags, -1, "query inherited descriptor flags");
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "inherited socket must be close-on-exec"
    );
}

fn launch(mode: &str) -> (Child, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let source = listener.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .env(CHILD_MODE, mode)
        .env("LISTEN_FDS", "1")
        .env("LISTEN_FD_NAMES", "http")
        .env_remove("WADDLE_VAPID_PRIVATE_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: The closure runs only in Command's forked child, so remapping fd 3
    // cannot invalidate parent-owned descriptors. The source listener stays open
    // through spawn. Only async-signal-safe dup2/fcntl calls and integer error
    // construction run before exec; Command exits the child if exec fails.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(source, 3) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // dup2(fd, fd) preserves CLOEXEC, so clear it explicitly in both cases.
            let flags = libc::fcntl(3, libc::F_GETFD);
            if flags == -1 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("launch listener fixture");
    drop(listener);
    (child, address)
}

fn assert_child_succeeded(child: Child) {
    let output = child.wait_with_output().expect("wait for listener fixture");
    assert!(
        output.status.success(),
        "listener fixture failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
