fn main() {
    // Capture the git commit SHA for embedding in the XEP-0092 version response.
    // Prefer GITHUB_SHA (set in GitHub Actions), then fall back to `git rev-parse HEAD`.
    let sha = std::env::var("GITHUB_SHA")
        .ok()
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout)
                            .ok()
                            .map(|s| s.trim().to_string())
                    } else {
                        None
                    }
                })
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=WADDLE_GIT_SHA={sha}");

    // Re-run if HEAD changes (branch switch, new commit).
    // Walk up from the manifest directory to locate .git/HEAD so the path is
    // correct regardless of workspace layout.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let git_head = std::path::Path::new(&manifest_dir)
        .ancestors()
        .find_map(|p| {
            let candidate = p.join(".git").join("HEAD");
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        });
    if let Some(head_path) = git_head {
        println!("cargo:rerun-if-changed={}", head_path.display());
    }
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
}
