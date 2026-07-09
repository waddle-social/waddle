use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const MATURITY: &[&str] = &["production", "beta", "experimental", "unsupported"];
const TOPOLOGIES: &[&str] = &["local", "federated"];
const MANIFEST_SOURCE: &str = include_str!("../../../capabilities.toml");

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
    advertised_features: Vec<String>,
    #[serde(default)]
    custom_namespaces: Vec<String>,
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
    use waddle_xmpp::disco::info::{
        call_features, community_service_features, muc_room_features, muc_service_features,
        pubsub_service_features, push_service_features, server_features, spaces_service_features,
        upload_service_features,
    };

    let mut advertised = BTreeSet::new();
    for feature in server_features()
        .into_iter()
        .chain(upload_service_features())
        .chain(pubsub_service_features())
        .chain(push_service_features())
        .chain(spaces_service_features())
        .chain(community_service_features())
        .chain(muc_service_features())
        .chain(muc_room_features(true, true, true, true, true))
        .chain(call_features())
    {
        advertised.insert(feature.0);
    }

    for capability in load_manifest().capability {
        for feature in capability
            .advertised_features
            .into_iter()
            .chain(capability.custom_namespaces)
        {
            assert!(
                advertised.contains(&feature),
                "{} claims feature absent from assembled runtime feature vectors: {feature}",
                capability.id
            );
        }
    }
}
