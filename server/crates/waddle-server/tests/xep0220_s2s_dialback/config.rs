//! The dialback configuration: every role off by default, the secret the
//! Initiating and Authoritative roles need, and inbound dialback only for
//! listed domains that are also admitted.

use waddle_server::s2s::config::{ConfigError, MIN_DIALBACK_SECRET_BYTES};

use crate::support::{domain, Pki, Spec, HOME_A, HOME_B};

/// The `[dialback]` table, with a secret file of `secret` when given.
fn dialback_table(pki: &Pki, outbound: bool, inbound: bool, secret: Option<&str>) -> Vec<String> {
    let mut lines = vec![
        "[dialback]".to_owned(),
        format!("outbound = {outbound}"),
        format!("inbound = {inbound}"),
    ];
    if let Some(secret) = secret {
        let path = pki.file("dialback.secret", secret);
        lines.push(format!("secret_file = {:?}", path.display().to_string()));
    }
    lines
}

#[tokio::test]
async fn every_dialback_role_is_off_by_default() {
    let pki = Pki::new();
    let config = Spec::new(HOME_A, HOME_B)
        .load(&pki)
        .await
        .expect("a configuration");
    assert!(!config.dialback().outbound());
    assert!(!config.dialback().inbound());
    assert!(config.dialback().secret().is_none());
    assert!(!config.permits_inbound_dialback(&domain(HOME_B)));
}

#[tokio::test]
async fn the_outbound_roles_need_a_long_enough_secret() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = dialback_table(&pki, true, false, None);
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::DialbackSecretMissing)
    ));

    let short = "s".repeat(MIN_DIALBACK_SECRET_BYTES - 1);
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = dialback_table(&pki, true, false, Some(&short));
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::DialbackSecretTooShort(
            MIN_DIALBACK_SECRET_BYTES
        ))
    ));
    // A trailing line ending is not part of the secret.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = dialback_table(&pki, true, false, Some(&format!("{short}\n")));
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::DialbackSecretTooShort(_))
    ));

    let secret = "s".repeat(MIN_DIALBACK_SECRET_BYTES);
    for text in [
        secret.clone(),
        format!("{secret}\n"),
        format!("{secret}\r\n"),
    ] {
        let mut spec = Spec::new(HOME_A, HOME_B);
        spec.tables = dialback_table(&pki, true, false, Some(&text));
        let config = spec
            .load(&pki)
            .await
            .expect("a configuration with a secret");
        assert!(config.dialback().outbound());
        assert!(config.dialback().secret().is_some());
        assert!(!config.dialback().inbound());
    }

    // A relative secret path fails to load.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = vec![
        "[dialback]".to_owned(),
        "outbound = true".to_owned(),
        "secret_file = \"dialback.secret\"".to_owned(),
    ];
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::RelativePath(_))
    ));

    // A secret without the outbound roles is loaded but unused.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = dialback_table(&pki, false, false, Some(&secret));
    let config = spec.load(&pki).await.expect("a configuration");
    assert!(!config.dialback().outbound());
    assert!(config.dialback().secret().is_none());
}

#[tokio::test]
async fn inbound_dialback_needs_the_role_the_list_and_admission() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "muc.home-b.test"];
    spec.dialback = vec![HOME_B];
    spec.tables = dialback_table(&pki, false, true, None);
    let config = spec.load(&pki).await.expect("a configuration");
    assert!(config.dialback().inbound());
    assert!(config.permits_inbound_dialback(&domain(HOME_B)));
    // Admitted but not listed.
    assert!(!config.permits_inbound_dialback(&domain("muc.home-b.test")));

    // Listed without the role.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.dialback = vec![HOME_B];
    let config = spec.load(&pki).await.expect("a configuration");
    assert!(!config.permits_inbound_dialback(&domain(HOME_B)));

    // Listed with the role, but never admitted.
    for mode in ["closed", "allowlist"] {
        let mut spec = Spec::new(HOME_A, HOME_B);
        spec.mode = mode;
        spec.allow = Vec::new();
        spec.dialback = vec![HOME_B];
        spec.tables = dialback_table(&pki, false, true, None);
        let config = spec.load(&pki).await.expect("a configuration");
        assert!(!config.permits_inbound_dialback(&domain(HOME_B)), "{mode}");
    }

    // A denylisted dialback entry is a contradiction.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.dialback = vec![HOME_B];
    spec.allow = Vec::new();
    spec.deny = vec![HOME_B];
    spec.tables = dialback_table(&pki, false, true, None);
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::Denied { .. })
    ));
}

#[tokio::test]
async fn unknown_dialback_settings_fail_to_load() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = vec![
        "[dialback]".to_owned(),
        "verify_plaintext = true".to_owned(),
    ];
    assert!(matches!(spec.load(&pki).await, Err(ConfigError::Syntax(_))));
}
