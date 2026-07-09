use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const MATURITY: &[&str] = &["production", "beta", "experimental", "unsupported"];
const TOPOLOGIES: &[&str] = &["local", "federated"];
const MANIFEST_SOURCE: &str = include_str!("../../../capabilities.toml");
const DISCO_TARGET_CONTRACT_SOURCE: &str = include_str!("../../../disco-target-contract.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    capability: Vec<Capability>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    id: String,
    server_maturity: String,
    #[serde(default = "default_availability")]
    availability: String,
    protocols: Vec<String>,
    advertised_features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    custom_namespaces: BTreeMap<String, Vec<String>>,
    server_evidence: Vec<String>,
    protocol_evidence: BTreeMap<String, Vec<String>>,
    topologies: BTreeMap<String, String>,
}

fn default_availability() -> String {
    "always".to_string()
}

fn server_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_manifest() -> Manifest {
    toml::from_str(MANIFEST_SOURCE).expect("parse embedded server capability manifest")
}

#[test]
fn checked_in_disco_target_contract_matches_the_rust_source_exactly() {
    let generated = waddle_server::server::disco_targets::target_contract_json()
        .expect("serialize disco target contract");
    assert_eq!(
        DISCO_TARGET_CONTRACT_SOURCE,
        format!("{generated}\n"),
        "regenerate server/disco-target-contract.json with \
         `waddle-server --disco-target-contract`"
    );
}

#[test]
fn capability_manifest_has_complete_unique_release_claims() {
    let manifest = load_manifest();
    assert_eq!(manifest.schema_version, 1);
    assert!(!manifest.capability.is_empty());

    let mut ids = BTreeSet::new();
    for capability in &manifest.capability {
        assert!(
            ids.insert(&capability.id),
            "duplicate capability {}",
            capability.id
        );
        assert!(
            capability
                .id
                .chars()
                .all(|character| character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || character == '-'),
            "capability ids must be lowercase kebab-case: {}",
            capability.id
        );
        assert!(MATURITY.contains(&capability.server_maturity.as_str()));
        assert!(["always", "configured"].contains(&capability.availability.as_str()));
        assert!(!capability.protocols.is_empty());
        assert!(
            !capability.advertised_features.is_empty(),
            "{} must assign every discovery claim to its owning target",
            capability.id
        );
        let mut target_claims: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (target_slug, features) in capability
            .advertised_features
            .iter()
            .chain(&capability.custom_namespaces)
        {
            let target = waddle_server::server::disco_targets::DiscoTarget::from_slug(target_slug)
                .unwrap_or_else(|| panic!("{} has unknown target {target_slug}", capability.id));
            if target.availability()
                == waddle_server::server::disco_targets::DiscoTargetAvailability::Configured
            {
                assert_eq!(
                    capability.availability, "configured",
                    "{} claims configured target {target_slug} as always available",
                    capability.id
                );
            }
            let claims = target_claims.entry(target_slug.as_str()).or_default();
            for feature in features {
                assert!(
                    !feature.is_empty(),
                    "{} has an empty feature",
                    capability.id
                );
                assert!(
                    claims.insert(feature.as_str()),
                    "{} repeats {feature} on {target_slug}",
                    capability.id
                );
            }
        }
        assert!(!capability.server_evidence.is_empty());
        assert_eq!(
            capability.protocols.iter().collect::<BTreeSet<_>>(),
            capability.protocol_evidence.keys().collect::<BTreeSet<_>>(),
            "every protocol on {} must have its own evidence",
            capability.id
        );
        assert_eq!(
            capability
                .topologies
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            TOPOLOGIES.iter().copied().collect()
        );
        for status in capability.topologies.values() {
            assert!(
                MATURITY.contains(&status.as_str()),
                "invalid maturity {status} on {}",
                capability.id
            );
        }
    }
}

fn assert_target_owns_feature(target_slug: &str, feature: &str) -> Result<(), String> {
    let target = waddle_server::server::disco_targets::DiscoTarget::from_slug(target_slug)
        .ok_or_else(|| format!("unknown disco target {target_slug}"))?;
    let target_features = waddle_server::server::disco_targets::manifest_target_features(target);
    target_features
        .iter()
        .any(|candidate| candidate.0 == feature)
        .then_some(())
        .ok_or_else(|| format!("{target_slug} does not advertise {feature}"))
}

#[test]
fn capability_manifest_references_vendored_xeps_and_existing_evidence() {
    let root = server_root();
    for capability in load_manifest().capability {
        for protocol in capability.protocols {
            protocol
                .strip_prefix("XEP-")
                .filter(|number| number.len() == 4 && number.chars().all(|c| c.is_ascii_digit()))
                .unwrap_or_else(|| panic!("invalid protocol {protocol} on {}", capability.id));
        }
        for (protocol, evidence_paths) in capability.protocol_evidence {
            assert!(
                !evidence_paths.is_empty(),
                "{} has no evidence for {protocol}",
                capability.id
            );
            for evidence in evidence_paths {
                let path = root.join(&evidence);
                assert!(
                    path.is_file(),
                    "{} has missing {protocol} evidence {evidence}",
                    capability.id
                );
                let source = fs::read_to_string(&path).expect("read capability evidence");
                match path.extension().and_then(|extension| extension.to_str()) {
                    Some("rs") => assert!(
                        source.contains("#[test]") || source.contains("#[tokio::test]"),
                        "{} evidence for {protocol} contains no Rust test: {evidence}",
                        capability.id
                    ),
                    Some("cue") => assert!(
                        source.contains(&protocol),
                        "{} CUE evidence does not declare {protocol}: {evidence}",
                        capability.id
                    ),
                    extension => panic!(
                        "{} evidence for {protocol} has unsupported extension {extension:?}: {evidence}",
                        capability.id
                    ),
                }
            }
        }
        for evidence in capability.server_evidence {
            let path = root.join(&evidence);
            assert!(
                path.is_file(),
                "{} has missing server evidence {evidence}",
                capability.id
            );
            let source = fs::read_to_string(&path).expect("read server evidence");
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("rs") => assert!(
                    source.contains("#[test]") || source.contains("#[tokio::test]"),
                    "{} server evidence contains no Rust test: {evidence}",
                    capability.id
                ),
                Some("cue") => assert!(
                    source.contains("steps:") && source.contains("xeps:"),
                    "{} server evidence is not an XEP scenario: {evidence}",
                    capability.id
                ),
                extension => panic!(
                    "{} server evidence has unsupported extension {extension:?}: {evidence}",
                    capability.id
                ),
            }
        }
    }
}

#[test]
fn advertised_feature_claims_exist_in_runtime_feature_vectors() {
    for capability in load_manifest().capability {
        for (target, features) in capability
            .advertised_features
            .into_iter()
            .chain(capability.custom_namespaces)
        {
            assert!(
                !features.is_empty(),
                "{} has an empty {target} claim",
                capability.id
            );
            for feature in features {
                assert_target_owns_feature(&target, &feature).unwrap_or_else(|error| {
                    panic!(
                        "{} has an invalid target-local claim: {error}",
                        capability.id
                    )
                });
            }
        }
    }
}

#[test]
fn target_validation_rejects_a_feature_found_only_in_the_global_union() {
    let muc = "http://jabber.org/protocol/muc";
    assert!(waddle_server::server::disco_targets::DiscoTarget::ALL
        .into_iter()
        .flat_map(waddle_server::server::disco_targets::manifest_target_features)
        .any(|feature| feature.0 == muc));
    assert_eq!(
        assert_target_owns_feature("server", muc),
        Err("server does not advertise http://jabber.org/protocol/muc".to_string())
    );
}
