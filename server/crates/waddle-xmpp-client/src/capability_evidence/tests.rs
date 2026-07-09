use std::sync::{Arc, Mutex};

use crate::discovery::{DiscoIdentity, DiscoInfoResult};

use super::args::parse_utc;
use super::collect::{
    collect_live_disco_with, sanitize_observation, CollectionProvenance, CollectionWindow,
    TargetInputs,
};
use super::contract::LoadedTargetContract;
use super::model::*;
use super::output::write_artifact;

const CONTRACT: &[u8] = include_bytes!("../../../../disco-target-contract.json");

fn contract() -> LoadedTargetContract {
    LoadedTargetContract::from_bytes(CONTRACT).expect("checked-in contract")
}

fn target_inputs(calls: bool, room: bool) -> TargetInputs {
    TargetInputs {
        xmpp_domain: "example.com".parse().expect("domain"),
        muc_domain: "muc.example.com".parse().expect("MUC domain"),
        spaces_domain: "spaces.example.com".parse().expect("Spaces domain"),
        account: "alice@example.com".parse().expect("account"),
        representative_muc_room: room
            .then(|| "private-room@muc.example.com".parse().expect("room")),
        calls_configured: calls,
    }
}

fn provenance() -> CollectionProvenance {
    CollectionProvenance {
        server_commit: GitCommit::parse("0123456789abcdef0123456789abcdef01234567".to_string())
            .expect("commit"),
        deployment_scope: DeploymentScope {
            job: ScopeLabel::parse("waddle-server".to_string()).expect("job"),
            environment: ScopeLabel::parse("production".to_string()).expect("environment"),
            cluster: ScopeLabel::parse("waddle-cloud".to_string()).expect("cluster"),
            namespace: ScopeLabel::parse("waddle".to_string()).expect("namespace"),
            expected_replicas: 2,
            identity_metric: MetricName::parse(DEFAULT_IDENTITY_METRIC.to_string())
                .expect("metric"),
            target_signal_id: SignalId::parse(DEFAULT_TARGET_SIGNAL_ID.to_string())
                .expect("signal"),
            identity_lookback_seconds: DEFAULT_IDENTITY_LOOKBACK_SECONDS,
        },
        window: CollectionWindow {
            start: parse_utc("2026-07-10T09:00:00Z").expect("start"),
            end: parse_utc("2026-07-10T10:00:00Z").expect("end"),
        },
    }
}

fn response_for(contract: &LoadedTargetContract, target: CapabilityTarget) -> DiscoInfoResult {
    let expectation = contract.targets.get(&target).expect("target expectation");
    let features = match expectation.observation_policy {
        ObservationPolicy::ExactWhenAvailable => expectation.claimable_features.clone(),
        ObservationPolicy::RuntimeExtensible => expectation.required_features.clone(),
        ObservationPolicy::RuntimeDependent => expectation
            .runtime_feature_variants
            .first()
            .expect("runtime variant")
            .clone(),
    };
    DiscoInfoResult {
        // Deliberately sensitive/transient fields: the artifact test below
        // proves neither survives serialization.
        jid: "private-room@muc.example.com".to_string(),
        node: None,
        identities: expectation
            .identities
            .iter()
            .map(|identity| DiscoIdentity {
                category: identity.category.0.clone(),
                identity_type: identity.type_.0.clone(),
                name: Some("Private Room Name".to_string()),
            })
            .collect(),
        features: features
            .iter()
            .rev()
            .map(|feature| feature.0.clone())
            .collect(),
        forms: Vec::new(),
    }
}

#[tokio::test]
async fn collection_queries_all_ten_targets_and_serializes_no_resolved_identity() {
    let contract = Arc::new(contract());
    let queried = Arc::new(Mutex::new(Vec::new()));
    let artifact = collect_live_disco_with(
        contract.as_ref(),
        &target_inputs(true, true),
        &provenance(),
        {
            let contract = Arc::clone(&contract);
            let queried = Arc::clone(&queried);
            move |target, _jid| {
                let response = response_for(contract.as_ref(), target);
                queried.lock().expect("query log").push(target);
                async move { Ok(response) }
            }
        },
        || parse_utc("2026-07-10T09:30:00Z").expect("capture"),
    )
    .await
    .expect("collect");
    assert_eq!(*queried.lock().expect("query log"), CapabilityTarget::ALL);
    let json = artifact.to_pretty_json().expect("serialize");
    assert!(!json.contains("private-room"));
    assert!(!json.contains("alice@example.com"));
    assert!(!json.contains("Private Room Name"));
    assert!(!json.contains("access_token"));
    assert_eq!(artifact.entities.len(), 10);
    assert!(artifact.skipped_targets.is_empty());
    assert!(artifact
        .entities
        .iter()
        .all(|entity| { entity.features.windows(2).all(|pair| pair[0] < pair[1]) }));
}

#[tokio::test]
async fn optional_targets_have_only_the_contract_allowed_skip_reasons() {
    let contract = Arc::new(contract());
    let artifact = collect_live_disco_with(
        contract.as_ref(),
        &target_inputs(false, false),
        &provenance(),
        {
            let contract = Arc::clone(&contract);
            move |target, _| {
                let response = response_for(contract.as_ref(), target);
                async move { Ok(response) }
            }
        },
        || parse_utc("2026-07-10T09:30:00Z").expect("capture"),
    )
    .await
    .expect("collect");
    assert_eq!(
        artifact.skipped_targets,
        vec![
            SkippedTarget {
                target: CapabilityTarget::CallsMixer,
                reason: SkipReason::NotConfigured,
            },
            SkippedTarget {
                target: CapabilityTarget::RepresentativeMucRoom,
                reason: SkipReason::NoRepresentativeEntity,
            },
        ]
    );
    assert_eq!(artifact.entities.len(), 8);
}

#[test]
fn checked_in_contract_has_the_exact_shared_target_set_and_unique_features() {
    let contract = contract();
    assert_eq!(
        contract.targets.keys().copied().collect::<Vec<_>>(),
        CapabilityTarget::ALL
    );
    for target in CapabilityTarget::ALL {
        let expectation = contract.targets.get(&target).expect("target");
        assert!(!expectation.required_features.is_empty());
        assert!(
            expectation
                .required_features
                .is_subset(&expectation.claimable_features),
            "{target}"
        );
    }
    assert_eq!(contract.sha256.0.len(), 64);
}

#[test]
fn contract_parsing_is_slug_keyed_not_array_order_dependent() {
    let mut document: serde_json::Value =
        serde_json::from_slice(CONTRACT).expect("checked-in contract JSON");
    document["targets"]
        .as_array_mut()
        .expect("target array")
        .reverse();
    let reordered = serde_json::to_vec(&document).expect("reordered contract JSON");

    let parsed = LoadedTargetContract::from_bytes(&reordered).expect("reordered target contract");
    assert_eq!(
        parsed.targets.keys().copied().collect::<Vec<_>>(),
        CapabilityTarget::ALL
    );
}

#[test]
fn runtime_dependent_observations_require_the_baseline_and_accept_registered_modes() {
    let contract = contract();

    for (target, optional) in [
        (
            CapabilityTarget::Server,
            "https://xmpp.org/extensions/isr/0",
        ),
        (CapabilityTarget::RepresentativeMucRoom, "muc_persistent"),
        (CapabilityTarget::RepresentativeMucRoom, "muc_membersonly"),
        (CapabilityTarget::RepresentativeMucRoom, "muc_hidden"),
        (CapabilityTarget::RepresentativeMucRoom, "muc_moderated"),
    ] {
        let expectation = contract.targets.get(&target).expect("target");
        let mut response = response_for(&contract, target);
        response.features = expectation
            .runtime_feature_variants
            .iter()
            .find(|variant| variant.iter().any(|feature| feature.0 == optional))
            .expect("registered runtime variant")
            .iter()
            .map(|feature| feature.0.clone())
            .collect();
        sanitize_observation(&contract, target, response).expect("registered runtime mode");
    }

    for target in [
        CapabilityTarget::Server,
        CapabilityTarget::RepresentativeMucRoom,
    ] {
        let mut response = response_for(&contract, target);
        let required = contract
            .targets
            .get(&target)
            .expect("target")
            .required_features
            .iter()
            .next()
            .expect("required feature")
            .0
            .clone();
        response.features.retain(|feature| feature != &required);
        assert!(matches!(
            sanitize_observation(&contract, target, response),
            Err(CapabilityEvidenceError::InvalidObservation { .. })
        ));
    }

    let mut impossible_room = response_for(&contract, CapabilityTarget::RepresentativeMucRoom);
    let opposite = if impossible_room
        .features
        .iter()
        .any(|feature| feature == "muc_open")
    {
        "muc_membersonly"
    } else {
        "muc_open"
    };
    impossible_room.features.push(opposite.to_string());
    assert!(matches!(
        sanitize_observation(
            &contract,
            CapabilityTarget::RepresentativeMucRoom,
            impossible_room,
        ),
        Err(CapabilityEvidenceError::InvalidObservation { .. })
    ));

    let mut partial_calls = response_for(&contract, CapabilityTarget::Server);
    partial_calls.features.push("urn:xmpp:jingle:1".to_string());
    assert!(matches!(
        sanitize_observation(&contract, CapabilityTarget::Server, partial_calls),
        Err(CapabilityEvidenceError::InvalidObservation { .. })
    ));

    let mut curated_extension = response_for(&contract, CapabilityTarget::Server);
    curated_extension
        .features
        .push("urn:waddle:link-board:1".to_string());
    sanitize_observation(&contract, CapabilityTarget::Server, curated_extension)
        .expect("curated extensions vary independently");
}

#[tokio::test]
async fn capture_outside_the_fixed_window_is_rejected() {
    let contract = Arc::new(contract());
    let error = collect_live_disco_with(
        contract.as_ref(),
        &target_inputs(false, false),
        &provenance(),
        {
            let contract = Arc::clone(&contract);
            move |target, _| {
                let response = response_for(contract.as_ref(), target);
                async move { Ok(response) }
            }
        },
        || parse_utc("2026-07-10T10:00:01Z").expect("capture"),
    )
    .await
    .expect_err("outside window");
    assert!(matches!(
        error,
        CapabilityEvidenceError::CapturedOutsideWindow
    ));
}

#[test]
fn runtime_extensible_targets_reject_every_feature_outside_the_checked_in_registry() {
    let contract = contract();
    for feature in [
        "urn:xmpp:sid:0",
        "urn:xmpp:invented:0",
        "urn:waddle:installed-extension:0",
    ] {
        let mut response = response_for(&contract, CapabilityTarget::MucService);
        response.features.push(feature.to_string());
        assert!(matches!(
            sanitize_observation(&contract, CapabilityTarget::MucService, response),
            Err(CapabilityEvidenceError::InvalidObservation {
                target: CapabilityTarget::MucService
            })
        ));
    }

    for target in [
        CapabilityTarget::Server,
        CapabilityTarget::RepresentativeMucRoom,
    ] {
        for feature in [
            "urn:xmpp:invented:0",
            "urn:waddle:extension:1",
            "urn:waddle:installed-extension:0",
        ] {
            let mut invalid = response_for(&contract, target);
            invalid.features.push(feature.to_string());
            assert!(matches!(
                sanitize_observation(&contract, target, invalid),
                Err(CapabilityEvidenceError::InvalidObservation { .. })
            ));
        }
    }
}

#[test]
fn waddle_feature_namespaces_reject_dynamic_or_encoded_values() {
    for feature in [
        "urn:waddle:extension:YWxpY2VAZXhhbXBsZS5jb20",
        "urn:waddle:account:550e8400-e29b-41d4-a716-446655440000",
        "urn:waddle:token:eyJhbGciOiJIUzI1NiJ9",
        "urn:waddle:extension:alice%40example.com:0",
        "urn:waddle:extension:alice@example.com:0",
    ] {
        assert!(
            FeatureNamespace::parse(feature.to_string()).is_err(),
            "{feature}"
        );
    }
    FeatureNamespace::parse("urn:waddle:extension:installed:0".to_string())
        .expect("static versioned namespace");
    FeatureNamespace::parse("https://xmpp.org/extensions/isr/0".to_string())
        .expect("pinned ISR namespace");
    for feature in [
        "https://xmpp.org/extensions/isr/0#alice",
        "https://xmpp.org/extensions/isr/1",
        "https://xmpp.org/users/alice",
    ] {
        assert!(FeatureNamespace::parse(feature.to_string()).is_err());
    }
}

#[test]
fn artifact_writer_refuses_overwrite_and_symlink_targets() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = directory.path().join("artifact.json");
    write_artifact(&output, "{}\n").expect("first write");
    assert!(matches!(
        write_artifact(&output, "{}\n"),
        Err(CapabilityEvidenceError::OutputExists)
    ));
}

#[test]
fn concurrent_artifact_writers_cannot_clobber_each_other() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output = Arc::new(directory.path().join("artifact.json"));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let writers = ["{\"writer\":1}\n", "{\"writer\":2}\n"].map(|contents| {
        let output = Arc::clone(&output);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            write_artifact(output.as_ref(), contents)
        })
    });
    let results = writers.map(|writer| writer.join().expect("writer joins"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(CapabilityEvidenceError::OutputExists)))
            .count(),
        1
    );
    let stored = std::fs::read_to_string(output.as_ref()).expect("stored artifact");
    assert!(matches!(
        stored.as_str(),
        "{\"writer\":1}\n" | "{\"writer\":2}\n"
    ));
}

#[cfg(unix)]
#[test]
fn artifact_writer_rejects_a_symlinked_parent_directory() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("tempdir");
    let real = directory.path().join("real");
    std::fs::create_dir(&real).expect("real directory");
    let linked = directory.path().join("linked");
    symlink(&real, &linked).expect("parent symlink");
    let error = write_artifact(&linked.join("artifact.json"), "{}\n")
        .expect_err("symlinked parent must fail");
    assert!(matches!(
        error,
        CapabilityEvidenceError::InvalidArgument("output path must not contain symlinked parents")
    ));
    assert!(!real.join("artifact.json").exists());
}
