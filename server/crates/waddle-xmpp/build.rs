//! Build script that captures the git commit SHA at compile time so the
//! server can advertise it through XEP-0092 Software Version.

use std::process::Command;

fn main() {
    let sha = std::env::var("WADDLE_GIT_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=WADDLE_GIT_SHA={sha}");
    println!("cargo:rerun-if-env-changed=WADDLE_GIT_SHA");
    // The workspace root is three levels up from this crate
    // (server/crates/waddle-xmpp/build.rs -> repo root).
    println!("cargo:rerun-if-changed=../../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../../.git/refs/heads");
    println!("cargo:rerun-if-changed=../../../.git/packed-refs");
}
