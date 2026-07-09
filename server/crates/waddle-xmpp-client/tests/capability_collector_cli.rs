use std::process::Command;

fn collector() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_waddle-capability-collector"));
    command.current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    command
}

fn valid_arguments(endpoint: &'static str) -> Vec<&'static str> {
    vec![
        "--endpoint",
        endpoint,
        "--xmpp-domain",
        "example.com",
        "--muc-domain",
        "muc.example.com",
        "--spaces-domain",
        "spaces.example.com",
        "--server-commit",
        "0123456789abcdef0123456789abcdef01234567",
        "--window-start",
        "2026-07-10T09:00:00Z",
        "--window-end",
        "2026-07-10T10:00:00Z",
        "--job",
        "waddle-server",
        "--environment",
        "production",
        "--cluster",
        "waddle-cloud",
        "--namespace",
        "waddle",
        "--expected-replicas",
        "2",
        "--target-contract",
        "disco-target-contract.json",
        "--output",
        "live-disco-export.json",
    ]
}

#[test]
fn help_exposes_environment_only_secret_input() {
    let output = collector()
        .arg("--help")
        .output()
        .expect("run collector help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(stdout.contains("--access-token-env"));
    assert!(!stdout.contains("--access-token "));
    assert!(stdout.contains("--account-env"));
    assert!(!stdout.contains("--account "));
    assert!(stdout.contains("--representative-muc-room-env"));
    assert!(!stdout.contains("--representative-muc-room "));
    assert!(stdout.contains("--calls-configured"));
}

#[test]
fn invalid_commit_fails_before_secret_or_network_access() {
    let mut arguments = valid_arguments("wss://chat.example.com/xmpp-websocket");
    let commit = arguments
        .iter()
        .position(|argument| *argument == "0123456789abcdef0123456789abcdef01234567")
        .expect("commit argument");
    arguments[commit] = "not-a-commit";
    let output = collector()
        .args(arguments)
        .env_remove("WADDLE_CAPABILITY_ACCESS_TOKEN")
        .output()
        .expect("run invalid collector");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("server commit must be a full lowercase Git SHA"));
    assert!(!stderr.contains("alice@example.com"));
}

#[test]
fn plaintext_or_credential_bearing_endpoints_are_rejected() {
    for endpoint in [
        "ws://chat.example.com/xmpp-websocket",
        "wss://user:secret@chat.example.com/xmpp-websocket",
        "wss://chat.example.com/xmpp-websocket#secret",
    ] {
        let output = collector()
            .args(valid_arguments(endpoint))
            .output()
            .expect("run collector");
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
        assert!(stderr.contains("credential-free wss endpoint"));
        assert!(!stderr.contains("user:secret"));
    }
}

#[test]
fn unrelated_websocket_host_is_rejected_before_secret_or_network_access() {
    let output = collector()
        .args(valid_arguments("wss://evil.example.net/xmpp-websocket"))
        .env("WADDLE_CAPABILITY_ACCOUNT_JID", "alice@example.com")
        .env("WADDLE_CAPABILITY_ACCESS_TOKEN", "must-not-leave-process")
        .output()
        .expect("run collector");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("must be the XMPP domain or one of its subdomains"));
    assert!(!stderr.contains("must-not-leave-process"));
}

#[test]
fn self_attested_endpoint_domain_cannot_override_the_authenticated_account_domain() {
    let mut arguments = valid_arguments("wss://attacker.example/xmpp-websocket");
    let domain = arguments
        .iter()
        .position(|argument| *argument == "--xmpp-domain")
        .expect("XMPP domain argument")
        + 1;
    arguments[domain] = "attacker.example";
    let output = collector()
        .args(arguments)
        .env("WADDLE_CAPABILITY_ACCOUNT_JID", "alice@example.com")
        .env("WADDLE_CAPABILITY_ACCESS_TOKEN", "must-not-leave-process")
        .output()
        .expect("run collector");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("account domain must match the XMPP domain"));
    assert!(!stderr.contains("must-not-leave-process"));
}

#[test]
fn mismatched_origin_is_rejected_before_secret_or_network_access() {
    let mut arguments = valid_arguments("wss://xmpp.example.com/xmpp-websocket");
    arguments.extend(["--origin", "https://example.com"]);
    let output = collector()
        .args(arguments)
        .env("WADDLE_CAPABILITY_ACCOUNT_JID", "alice@example.com")
        .env("WADDLE_CAPABILITY_ACCESS_TOKEN", "must-not-leave-process")
        .output()
        .expect("run collector");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("WebSocket endpoint host and port"));
    assert!(!stderr.contains("must-not-leave-process"));
}

#[test]
fn trusted_subdomain_reaches_token_validation_without_network_access() {
    let output = collector()
        .args(valid_arguments("wss://xmpp.example.com/xmpp-websocket"))
        .env("WADDLE_CAPABILITY_ACCOUNT_JID", "alice@example.com")
        .env_remove("WADDLE_CAPABILITY_ACCESS_TOKEN")
        .output()
        .expect("run collector");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("access token environment variable is missing"));
}

#[test]
fn sensitive_jid_environment_values_never_appear_in_errors() {
    let sensitive = "sensitive-account@example.com/private-resource";
    let output = collector()
        .args(valid_arguments("wss://chat.example.com/xmpp-websocket"))
        .env("WADDLE_CAPABILITY_ACCOUNT_JID", sensitive)
        .output()
        .expect("run collector");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("JID environment value is invalid"));
    assert!(!stderr.contains(sensitive));
}

#[test]
fn timeout_and_window_bounds_are_enforced_before_network_access() {
    let mut timeout_arguments = valid_arguments("wss://chat.example.com/xmpp-websocket");
    timeout_arguments.extend(["--request-timeout-seconds", "61"]);
    let timeout = collector()
        .args(timeout_arguments)
        .env("WADDLE_CAPABILITY_ACCOUNT_JID", "alice@example.com")
        .output()
        .expect("run collector");
    assert!(!timeout.status.success());
    assert!(String::from_utf8(timeout.stderr)
        .expect("stderr")
        .contains("invalid replica, lookback, or timeout value"));

    let mut short_window = valid_arguments("wss://chat.example.com/xmpp-websocket");
    let end = short_window
        .iter()
        .position(|argument| *argument == "2026-07-10T10:00:00Z")
        .expect("window end");
    short_window[end] = "2026-07-10T09:59:59Z";
    let window = collector()
        .args(short_window)
        .env("WADDLE_CAPABILITY_ACCOUNT_JID", "alice@example.com")
        .output()
        .expect("run collector");
    assert!(!window.status.success());
    assert!(String::from_utf8(window.stderr)
        .expect("stderr")
        .contains("at least 60 minutes"));
}
