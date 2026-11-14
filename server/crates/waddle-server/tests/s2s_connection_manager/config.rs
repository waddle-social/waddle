//! The configuration snapshot: activation, D1 admission with the dialback
//! and trusted-home lists, refused domain forms and load failures.

use std::path::PathBuf;

use waddle_server::s2s::config::{
    AdmissionMode, AdmissionRefusal, ConfigError, ConfigSection, S2sActivation,
    MAX_MATERIAL_FILE_BYTES,
};
use waddle_server::s2s::verify::TrustProfileKind;

use crate::support::{domain, load_text, Pki, Spec, HOME_A, HOME_B};

/// A change to a deployment's specification.
type Adjust = fn(&mut Spec);

#[test]
fn s2s_is_off_unless_explicitly_enabled() {
    let file = Some(PathBuf::from("/etc/waddle/s2s.toml"));
    assert_eq!(
        S2sActivation::from_values(None, file.clone()).expect("off"),
        S2sActivation::Disabled
    );
    assert_eq!(
        S2sActivation::from_values(Some("false"), file.clone()).expect("off"),
        S2sActivation::Disabled
    );
    assert_eq!(
        S2sActivation::from_values(Some("true"), file.clone()).expect("on"),
        S2sActivation::Enabled(PathBuf::from("/etc/waddle/s2s.toml"))
    );
    assert!(matches!(
        S2sActivation::from_values(Some("true"), None),
        Err(ConfigError::MissingConfigFile)
    ));
    assert!(matches!(
        S2sActivation::from_values(Some("true"), Some(PathBuf::from("s2s.toml"))),
        Err(ConfigError::RelativeConfigFile)
    ));
    assert!(matches!(
        S2sActivation::from_values(Some("yes"), file),
        Err(ConfigError::InvalidEnabledFlag)
    ));
}

#[tokio::test]
async fn a_valid_snapshot_loads_with_allowlist_as_the_default_mode() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.mode = "allowlist";
    let text = spec.toml(&pki).replace("mode = \"allowlist\"\n", "");
    let config = load_text(&pki, &text).await.expect("a valid configuration");
    assert_eq!(config.admission().mode(), AdmissionMode::Allowlist);
    assert_eq!(config.local_domains(), &[domain(HOME_A)]);
    assert!(config.credentials(&domain(HOME_A)).is_some());
    assert_eq!(
        config.trust().profile_for(&domain(HOME_B)).kind(),
        TrustProfileKind::PrivateCa
    );
    assert_eq!(
        config.trust().profile_for(&domain("other.test")).kind(),
        TrustProfileKind::PublicWebPki
    );
}

#[tokio::test]
async fn allowlist_entries_match_exactly_and_denylist_entries_cover_subdomains() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "example.test"];
    spec.deny = vec!["evil.test"];
    let config = spec.load(&pki).await.expect("a valid configuration");
    let admission = config.admission();
    assert_eq!(admission.decide(&domain(HOME_B)), Ok(()));
    assert_eq!(admission.decide(&domain("example.test")), Ok(()));
    // An allowlist entry says nothing about its subdomains.
    assert_eq!(
        admission.decide(&domain("muc.example.test")),
        Err(AdmissionRefusal::NotAllowlisted)
    );
    // A denylist entry covers its domain and subdomains, label by label.
    assert_eq!(
        admission.decide(&domain("evil.test")),
        Err(AdmissionRefusal::Denylisted)
    );
    assert_eq!(
        admission.decide(&domain("muc.evil.test")),
        Err(AdmissionRefusal::Denylisted)
    );
    assert!(!admission.is_denylisted(&domain("badevil.test")));
    // Local domains are never remote peers.
    assert_eq!(
        admission.decide(&domain(HOME_A)),
        Err(AdmissionRefusal::LocalDomain)
    );
}

#[tokio::test]
async fn the_denylist_applies_in_every_mode() {
    let pki = Pki::new();
    for mode in ["closed", "allowlist", "open"] {
        let mut spec = Spec::new(HOME_A, HOME_B);
        spec.mode = mode;
        spec.deny = vec!["evil.test"];
        let config = spec.load(&pki).await.expect("a valid configuration");
        assert_eq!(
            config.admission().decide(&domain("a.evil.test")),
            Err(AdmissionRefusal::Denylisted),
            "{mode}"
        );
    }

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.mode = "open";
    spec.deny = vec!["evil.test"];
    let config = spec.load(&pki).await.expect("open mode");
    assert_eq!(config.admission().decide(&domain("anyone.test")), Ok(()));

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.mode = "closed";
    let config = spec.load(&pki).await.expect("closed mode");
    assert_eq!(
        config.admission().decide(&domain(HOME_B)),
        Err(AdmissionRefusal::Closed)
    );
}

#[tokio::test]
async fn ip_addresses_numeric_ipv4_forms_and_single_label_names_are_never_admitted() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.mode = "open";
    let config = spec.load(&pki).await.expect("open mode");
    for refused in [
        "127.0.0.1",
        "[::1]",
        "2130706433",
        "0x7f.1",
        "127.1",
        "localhost",
    ] {
        assert_eq!(
            config.admission().decide(&domain(refused)),
            Err(AdmissionRefusal::NotAPeerDomain),
            "{refused}"
        );
    }
    assert_eq!(
        config.admission().decide(&domain("example.org")),
        Err(AdmissionRefusal::OutsideTestDomain)
    );
}

#[tokio::test]
async fn a_list_entry_the_denylist_covers_fails_to_load() {
    let pki = Pki::new();
    let cases: [(Adjust, ConfigSection); 3] = [
        (
            |spec| {
                spec.allow = vec!["muc.evil.test"];
                spec.deny = vec!["evil.test"];
            },
            ConfigSection::Allowlist,
        ),
        (
            |spec| {
                spec.dialback = vec!["evil.test"];
                spec.deny = vec!["evil.test"];
            },
            ConfigSection::DialbackList,
        ),
        (
            |spec| {
                spec.trusted_homes = vec!["a.b.evil.test"];
                spec.deny = vec!["evil.test"];
            },
            ConfigSection::TrustedHomes,
        ),
    ];
    for (adjust, section) in cases {
        let mut spec = Spec::new(HOME_A, HOME_B);
        adjust(&mut spec);
        match spec.load(&pki).await {
            Err(ConfigError::Denied {
                section: refused,
                index: 0,
            }) => assert_eq!(refused, section),
            other => panic!("{section}: expected a denied entry, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn refused_domain_forms_in_a_list_fail_to_load() {
    let pki = Pki::new();
    for entry in ["127.0.0.1", "[::1]", "127.1", "0x7f.1", "localhost"] {
        let mut spec = Spec::new(HOME_A, HOME_B);
        spec.allow = vec![entry];
        assert!(
            matches!(
                spec.load(&pki).await,
                Err(ConfigError::NotAPeerDomain {
                    section: ConfigSection::Allowlist,
                    index: 0
                })
            ),
            "{entry}"
        );
    }
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.deny = vec!["2130706433"];
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::NotAPeerDomain {
            section: ConfigSection::Denylist,
            ..
        })
    ));
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec!["example.org"];
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::OutsideTestDomain { .. })
    ));
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec!["not a domain"];
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::InvalidDomain { .. })
    ));
}

#[tokio::test]
async fn contradictory_or_incomplete_configurations_fail_to_load() {
    let pki = Pki::new();
    let pki = &pki;
    let check = |spec: Spec, expected: fn(&ConfigError) -> bool, label: &'static str| async move {
        match spec.load(pki).await {
            Err(error) => assert!(expected(&error), "{label}: {error:?}"),
            Ok(_) => panic!("{label}: the configuration loaded"),
        }
    };

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, HOME_B];
    check(
        spec,
        |error| matches!(error, ConfigError::Duplicate { index: 1, .. }),
        "duplicate entry",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_A];
    check(
        spec,
        |error| matches!(error, ConfigError::LocalDomain { .. }),
        "local domain on the allowlist",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.deny = vec!["test.test"];
    spec.local = vec!["muc.test.test"];
    check(
        spec,
        |error| {
            matches!(
                error,
                ConfigError::LocalDomain {
                    section: ConfigSection::Denylist,
                    ..
                }
            )
        },
        "denylist covering a local domain",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.listen = Some("192.0.2.1:5269".to_owned());
    check(
        spec,
        |error| matches!(error, ConfigError::NotLoopback(ConfigSection::Listen)),
        "listener outside loopback",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.endpoints = vec![(HOME_B, "198.51.100.7:5269".parse().expect("address"))];
    check(
        spec,
        |error| matches!(error, ConfigError::NotLoopback(ConfigSection::Endpoints)),
        "endpoint outside loopback",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec!["max_sockets = 33".to_owned()];
    check(
        spec,
        |error| matches!(error, ConfigError::LimitOutOfRange("max_sockets")),
        "a cap above its ceiling",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec!["stage_timeout_ms = 0".to_owned()];
    check(
        spec,
        |error| matches!(error, ConfigError::LimitOutOfRange("stage_timeout_ms")),
        "a zero deadline",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.require_private_ca = vec![HOME_B];
    check(
        spec,
        |error| matches!(error, ConfigError::NotTrustedHome { index: 0 }),
        "private CA required of a home that is not trusted",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "public.test"];
    spec.trusted_homes = vec!["public.test"];
    spec.require_private_ca = vec!["public.test"];
    check(
        spec,
        |error| matches!(error, ConfigError::NoPrivateCaProfile { index: 0 }),
        "private CA required of a home without a profile",
    )
    .await;
}

#[tokio::test]
async fn malformed_files_fail_to_load() {
    let pki = Pki::new();
    let valid = Spec::new(HOME_A, HOME_B).toml(&pki);

    let unknown = format!("{valid}\nunknown_setting = 1");
    assert!(matches!(
        load_text(&pki, &unknown).await,
        Err(ConfigError::Syntax(_))
    ));

    let generation = valid.replace("0b8f2d4e-6a59-4f1c-9a43-3f1d2c7e5a10", "not-a-generation");
    assert!(matches!(
        load_text(&pki, &generation).await,
        Err(ConfigError::InvalidGeneration)
    ));

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.local = vec![HOME_A, "muc.home-a.test"];
    let text = spec.toml(&pki);
    // Drop the second domain's credentials.
    let cut = text
        .match_indices("[[credentials]]")
        .nth(1)
        .map(|(index, _)| index)
        .expect("two credential tables");
    let end = text[cut..].find("[[private_ca]]").expect("a profile table") + cut;
    let missing = [&text[..cut], &text[end..]].concat();
    assert!(matches!(
        load_text(&pki, &missing).await,
        Err(ConfigError::MissingCredentials { index: 1 })
    ));

    let ca = pki.ca_path.display().to_string();
    let relative = valid.replace(&ca, "ca.pem");
    assert!(matches!(
        load_text(&pki, &relative).await,
        Err(ConfigError::RelativePath(ConfigSection::PrivateCa))
    ));

    let large = pki.file(
        "large.pem",
        &"A".repeat(usize::try_from(MAX_MATERIAL_FILE_BYTES).expect("fits") + 1),
    );
    let oversized = valid.replace(&ca, &large.display().to_string());
    assert!(matches!(
        load_text(&pki, &oversized).await,
        Err(ConfigError::MaterialTooLarge(ConfigSection::PrivateCa))
    ));

    let not_pem = pki.file("not-pem.pem", "not a certificate");
    let garbage = valid.replace(&ca, &not_pem.display().to_string());
    assert!(matches!(
        load_text(&pki, &garbage).await,
        Err(ConfigError::NoPem { .. })
    ));

    let padded = format!("{valid}\n#{}", "x".repeat(64 * 1024));
    assert!(matches!(
        load_text(&pki, &padded).await,
        Err(ConfigError::ConfigFileTooLarge)
    ));
}

#[tokio::test]
async fn a_key_that_does_not_belong_to_the_certificate_fails_to_load() {
    let pki = Pki::new();
    let text = Spec::new(HOME_A, HOME_B).toml(&pki);
    let (_, other_key) = pki.credentials("other", &[HOME_A]);
    let key_line = text
        .lines()
        .find(|line| line.starts_with("private_key"))
        .expect("a key line")
        .to_owned();
    let swapped = text.replace(
        &key_line,
        &format!("private_key = {:?}", other_key.display().to_string()),
    );
    assert!(matches!(
        load_text(&pki, &swapped).await,
        Err(ConfigError::Credentials { index: 0, .. })
    ));
}

#[tokio::test]
async fn trusted_homes_are_trusted_on_the_public_profile_unless_a_private_ca_is_required() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "public.test"];
    spec.trusted_homes = vec![HOME_B, "public.test"];
    spec.require_private_ca = vec![HOME_B];
    let config = spec.load(&pki).await.expect("a valid configuration");
    let admission = config.admission();
    assert!(admission.is_trusted_home(&domain("public.test"), TrustProfileKind::PublicWebPki));
    assert!(admission.is_trusted_home(&domain(HOME_B), TrustProfileKind::PrivateCa));
    assert!(!admission.is_trusted_home(&domain(HOME_B), TrustProfileKind::PublicWebPki));
    assert!(admission.requires_private_ca(&domain(HOME_B)));
    // Being admitted does not make a domain a trusted home.
    assert!(!admission.is_trusted_home(&domain("other.test"), TrustProfileKind::PrivateCa));
}

#[tokio::test]
async fn dialback_and_trusted_home_lists_never_admit_a_domain() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.dialback = vec!["dialback.test"];
    spec.trusted_homes = vec!["trusted.test"];
    let config = spec.load(&pki).await.expect("a valid configuration");
    let admission = config.admission();
    assert_eq!(
        admission.decide(&domain("dialback.test")),
        Err(AdmissionRefusal::NotAllowlisted)
    );
    assert!(!admission.permits_dialback(&domain("dialback.test")));
    assert!(!admission.is_trusted_home(&domain("trusted.test"), TrustProfileKind::PrivateCa));
}
