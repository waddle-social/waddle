//! The configuration snapshot: activation and startup checks, D1
//! admission with the dialback and trusted-home lists, refused domain
//! forms, load failures and the default resource model.

use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;
use std::path::PathBuf;

use jid::{BareJid, DomainPart};
use waddle_server::s2s::config::{
    AdmissionMode, AdmissionRefusal, ConfigError, ConfigSection, RateLimit, ResourceLimits,
    S2sActivation, CONFIG_FILE_VARIABLE, ENABLED_VARIABLE, MAX_INCOMPLETE_ELEMENT_TIME,
    MAX_MATERIAL_FILE_BYTES, MIN_INPUT_BYTES_PER_SECOND, STREAM_CODEC_LIMITS,
};
use waddle_server::s2s::conn::{
    check_inherited_listener, load_config, load_enabled_config_from, StartupError,
};
use waddle_server::s2s::verify::TrustProfileKind;
use waddle_server::server::XmppConfig;

use crate::support::{domain, load_text, settings, Pki, Spec, HOME_A, HOME_B};

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
        S2sActivation::from_values(Some("yes"), file.clone()),
        Err(ConfigError::InvalidEnabledFlag)
    ));
    assert_eq!(
        S2sActivation::from_os_values(Some(OsStr::new("true")), file.clone()).expect("on"),
        S2sActivation::Enabled(PathBuf::from("/etc/waddle/s2s.toml"))
    );
    assert_eq!(
        S2sActivation::from_os_values(None, file).expect("off"),
        S2sActivation::Disabled
    );
}

#[cfg(unix)]
#[test]
fn an_enable_flag_that_is_not_unicode_fails_instead_of_reading_as_unset() {
    use std::os::unix::ffi::OsStrExt;

    let file = Some(PathBuf::from("/etc/waddle/s2s.toml"));
    assert!(matches!(
        S2sActivation::from_os_values(Some(OsStr::from_bytes(b"tru\xff")), file),
        Err(ConfigError::InvalidEnabledFlag)
    ));
}

/// The XMPP configuration of a process serving `domain` with its default
/// MUC and Spaces services.
fn served(domain: &str) -> XmppConfig {
    XmppConfig {
        domain: domain.to_owned(),
        muc_domain: DomainPart::new(&["muc.", domain].concat())
            .expect("a MUC domain")
            .into_owned(),
        spaces_jid: BareJid::new(&["spaces.", domain].concat()).expect("a Spaces JID"),
        ..XmppConfig::default()
    }
}

#[tokio::test]
async fn startup_reads_nothing_while_s2s_is_off_and_checks_the_served_domains() {
    let pki = Pki::new();
    // While S2S is off no file is read, so a missing one is no error.
    let off = load_config(S2sActivation::Disabled, &served(HOME_A), settings())
        .await
        .expect("S2S off");
    assert!(off.is_none());

    // The account, MUC and Spaces domains this process serves.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.local = vec![HOME_A, "muc.home-a.test", "spaces.home-a.test"];
    let path = pki.file("served.toml", &spec.toml(&pki));
    let config = load_config(
        S2sActivation::Enabled(path.clone()),
        &served(HOME_A),
        settings(),
    )
    .await
    .expect("served local domains")
    .expect("S2S on");
    assert_eq!(config.local_domains().len(), 3);

    // A local domain this process does not serve fails startup.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.local = vec![HOME_A, "other.home-a.test"];
    let path = pki.file("unserved.toml", &spec.toml(&pki));
    let unserved = load_config(S2sActivation::Enabled(path), &served(HOME_A), settings()).await;
    assert!(
        matches!(
            unserved,
            Err(StartupError::UnservedLocalDomain { index: 1 })
        ),
        "{unserved:?}"
    );

    // An enabled S2S without a readable file fails startup.
    let missing = load_config(
        S2sActivation::Enabled(pki.dir.path().join("missing.toml")),
        &served(HOME_A),
        settings(),
    )
    .await;
    assert!(
        matches!(
            missing,
            Err(StartupError::Config(ConfigError::ConfigFileUnreadable))
        ),
        "{missing:?}"
    );
}

/// The activation variables as `pairs` set them and nothing else.
fn variables(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| OsString::from(value))
    }
}

#[tokio::test]
async fn the_process_entry_point_is_off_without_the_flag() {
    // The variables are given explicitly, so the process environment the
    // test runs in plays no part.
    let pki = Pki::new();
    let xmpp = served(HOME_A);
    let missing_file = pki.dir.path().join("missing.toml");
    let missing_file = missing_file.to_str().expect("a UTF-8 path");
    for unset in [
        variables(&[]),
        variables(&[(ENABLED_VARIABLE, "false")]),
        // A file named while the flag is unset or false is never read.
        variables(&[(CONFIG_FILE_VARIABLE, missing_file)]),
        variables(&[
            (ENABLED_VARIABLE, "false"),
            (CONFIG_FILE_VARIABLE, missing_file),
        ]),
    ] {
        let config = load_enabled_config_from(&xmpp, unset)
            .await
            .expect("S2S off");
        assert!(config.is_none());
    }

    // The flag without a file fails startup.
    let enabled = load_enabled_config_from(&xmpp, variables(&[(ENABLED_VARIABLE, "true")])).await;
    assert!(
        matches!(
            enabled,
            Err(StartupError::Config(ConfigError::MissingConfigFile))
        ),
        "{enabled:?}"
    );

    // The flag and a valid file start S2S with that file. The fixture's
    // certificates are valid only at the fixed test time, so the file the
    // variables name is loaded with the test clock rather than the
    // process's.
    let path = pki.file("entry.toml", &Spec::new(HOME_A, HOME_B).toml(&pki));
    let path = path.to_str().expect("a UTF-8 path");
    let activation = S2sActivation::from_variables(variables(&[
        (ENABLED_VARIABLE, "true"),
        (CONFIG_FILE_VARIABLE, path),
    ]))
    .expect("S2S on");
    let config = load_config(activation, &xmpp, settings())
        .await
        .expect("a valid configuration")
        .expect("S2S on");
    assert_eq!(config.local_domains(), &[domain(HOME_A)]);
}

#[test]
fn an_inherited_listener_must_be_on_the_configured_loopback_address() {
    let address = |text: &str| -> SocketAddr { text.parse().expect("a socket address") };
    let configured = address("127.0.0.1:5269");
    assert!(check_inherited_listener(configured, address("127.0.0.1:5269")).is_ok());
    for actual in [
        "0.0.0.0:5269",
        "192.0.2.1:5269",
        "127.0.0.2:5269",
        "127.0.0.1:5270",
        "[::1]:5269",
        "[::]:5269",
    ] {
        assert!(
            matches!(
                check_inherited_listener(configured, address(actual)),
                Err(StartupError::InheritedListenerMismatch)
            ),
            "{actual}"
        );
    }
    // Port zero, used only by the native fixture, matches the port the
    // first process bound, but never another address.
    let any_port = address("127.0.0.1:0");
    assert!(check_inherited_listener(any_port, address("127.0.0.1:40000")).is_ok());
    assert!(check_inherited_listener(any_port, address("0.0.0.0:40000")).is_err());
    assert!(check_inherited_listener(address("[::1]:5269"), address("[::1]:5269")).is_ok());
}

#[test]
fn the_default_caps_leave_room_for_negotiation() {
    let limits = ResourceLimits::default();
    // Established streams never take the sockets that unauthenticated
    // connections need, and peers stay below each direction's socket cap.
    assert!(limits.max_streams + limits.max_unauthenticated <= limits.max_sockets);
    assert!(limits.max_streams < limits.max_sockets);
    assert!(limits.max_peers < limits.max_inbound_sockets);
    assert!(limits.max_peers < limits.max_outbound_sockets);
    assert!(limits.max_peers <= limits.max_streams);
    // One source address cannot hold every inbound handshake.
    assert!(limits.max_unauthenticated_per_address < limits.max_inbound_handshakes);
    // Four caps default below ceilings an operator may raise them to.
    let ceiling = ResourceLimits::CEILING;
    assert_eq!(limits.max_unauthenticated_per_address, 2);
    assert_eq!(ceiling.max_unauthenticated_per_address, 4);
    assert_eq!(limits.max_negotiating_per_peer, 3);
    assert_eq!(ceiling.max_negotiating_per_peer, 4);
    // A clustered peer's streams for one pair, inbound only.
    assert_eq!(limits.max_inbound_streams_per_peer, 4);
    assert_eq!(ceiling.max_inbound_streams_per_peer, 8);
    assert!(limits.max_inbound_streams_per_peer <= limits.max_streams);
    // The router's pair queues: more of them than peers can hold streams,
    // since a pair is directional and a queue outlives its stream.
    assert_eq!(limits.max_pair_queues, 64);
    assert_eq!(ceiling.max_pair_queues, 256);
    assert!(limits.max_pair_queues >= limits.max_peers);
}

#[test]
fn even_the_lowest_input_rate_reads_a_ceiling_stanza_within_the_element_bound() {
    let ceiling = u64::try_from(STREAM_CODEC_LIMITS.max_stanza_bytes).expect("a size");
    let floor = u64::from(MIN_INPUT_BYTES_PER_SECOND);
    assert!(ceiling.div_ceil(floor) < MAX_INCOMPLETE_ELEMENT_TIME.as_secs());
    let limits = ResourceLimits::default();
    for rate in [limits.input_rate_per_socket, limits.input_rate_global] {
        assert!(rate.per_second >= MIN_INPUT_BYTES_PER_SECOND);
    }
}

#[tokio::test]
async fn limits_load_within_their_ranges() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec![
        "max_unauthenticated_per_address = 4".to_owned(),
        "max_negotiating_per_peer = 4".to_owned(),
        "max_inbound_streams_per_peer = 8".to_owned(),
        "max_pair_queues = 128".to_owned(),
        "input_per_socket_per_second = 32768".to_owned(),
        "input_per_socket_burst = 16384".to_owned(),
        "input_per_second = 65536".to_owned(),
        "input_burst = 65536".to_owned(),
    ];
    let config = spec.load(&pki).await.expect("limits within their ranges");
    let limits = config.limits();
    assert_eq!(limits.max_unauthenticated_per_address, 4);
    assert_eq!(limits.max_negotiating_per_peer, 4);
    assert_eq!(limits.max_inbound_streams_per_peer, 8);
    assert_eq!(limits.max_pair_queues, 128);
    assert_eq!(
        limits.input_rate_per_socket,
        RateLimit {
            per_second: 32 * 1024,
            burst: 16 * 1024,
        }
    );
    assert_eq!(
        limits.input_rate_global,
        RateLimit {
            per_second: 64 * 1024,
            burst: 64 * 1024,
        }
    );

    let config = Spec::new(HOME_A, HOME_B)
        .load(&pki)
        .await
        .expect("the defaults");
    assert_eq!(config.limits(), &ResourceLimits::default());
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
    spec.limits = vec!["max_negotiating_per_peer = 5".to_owned()];
    check(
        spec,
        |error| {
            matches!(
                error,
                ConfigError::LimitOutOfRange("max_negotiating_per_peer")
            )
        },
        "a per-peer budget above its ceiling",
    )
    .await;

    for beyond in [
        "max_inbound_streams_per_peer = 9",
        "max_inbound_streams_per_peer = 0",
    ] {
        let mut spec = Spec::new(HOME_A, HOME_B);
        spec.limits = vec![beyond.to_owned()];
        check(
            spec,
            |error| {
                matches!(
                    error,
                    ConfigError::LimitOutOfRange("max_inbound_streams_per_peer")
                )
            },
            "the inbound streams per pair outside their range",
        )
        .await;
    }

    for beyond in ["max_pair_queues = 257", "max_pair_queues = 0"] {
        let mut spec = Spec::new(HOME_A, HOME_B);
        spec.limits = vec![beyond.to_owned()];
        check(
            spec,
            |error| matches!(error, ConfigError::LimitOutOfRange("max_pair_queues")),
            "the router's pair queues outside their range",
        )
        .await;
    }

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec!["input_per_socket_per_second = 1024".to_owned()];
    check(
        spec,
        |error| {
            matches!(
                error,
                ConfigError::LimitOutOfRange("input_per_socket_per_second")
            )
        },
        "an input rate below its floor",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec!["input_per_second = 1048576".to_owned()];
    check(
        spec,
        |error| matches!(error, ConfigError::LimitOutOfRange("input_per_second")),
        "an input rate above its ceiling",
    )
    .await;

    // The default 24 streams and 8 unauthenticated sockets need 32.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec!["max_sockets = 31".to_owned()];
    check(
        spec,
        |error| matches!(error, ConfigError::InconsistentLimit("max_streams")),
        "streams that would take the sockets negotiation needs",
    )
    .await;

    // The default peer cap is 12.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec!["max_inbound_sockets = 12".to_owned()];
    check(
        spec,
        |error| matches!(error, ConfigError::InconsistentLimit("max_peers")),
        "a peer cap not below the inbound socket cap",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec!["input_per_second = 32768".to_owned()];
    check(
        spec,
        |error| matches!(error, ConfigError::InconsistentLimit("input_per_second")),
        "a global input rate below the per-socket rate",
    )
    .await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec!["input_burst = 65536".to_owned()];
    check(
        spec,
        |error| matches!(error, ConfigError::InconsistentLimit("input_burst")),
        "a global input burst below the per-socket burst",
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
async fn the_denylist_may_name_domains_outside_the_test_domain() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.deny = vec!["evil.example"];
    let config = spec.load(&pki).await.expect("a valid configuration");
    let admission = config.admission();
    for denied in ["evil.example", "muc.evil.example"] {
        assert_eq!(
            admission.decide(&domain(denied)),
            Err(AdmissionRefusal::Denylisted),
            "{denied}"
        );
    }
    assert_eq!(
        admission.decide(&domain("fine.example")),
        Err(AdmissionRefusal::OutsideTestDomain)
    );
    // Lists that admit or trust still may not.
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.trusted_homes = vec!["home.example"];
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::OutsideTestDomain {
            section: ConfigSection::TrustedHomes,
            index: 0
        })
    ));
}

#[tokio::test]
async fn only_open_mode_holds_back_the_reserved_share_of_the_caps() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.mode = "open";
    spec.trusted_homes = vec!["trusted.test"];
    let config = spec.load(&pki).await.expect("open mode");
    let admission = config.admission();
    assert!(admission.is_reserved_peer(&domain(HOME_B)));
    assert!(admission.is_reserved_peer(&domain("trusted.test")));
    assert!(!admission.is_reserved_peer(&domain("anyone.test")));

    let config = Spec::new(HOME_A, HOME_B)
        .load(&pki)
        .await
        .expect("allowlist mode");
    assert!(config.admission().is_reserved_peer(&domain(HOME_B)));
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
