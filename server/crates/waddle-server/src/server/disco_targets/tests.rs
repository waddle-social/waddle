use std::collections::BTreeSet;

use waddle_xmpp::disco::Feature;

use super::*;

#[test]
fn target_contract_is_stable_complete_and_contains_no_resolved_jid() {
    let json = target_contract_json().expect("serialize target contract");
    assert_eq!(
        json,
        target_contract_json().expect("serialize target contract twice")
    );
    for target in DiscoTarget::ALL {
        assert!(json.contains(&format!("\"slug\": \"{}\"", target.slug())));
    }
    assert_eq!(json.matches("\"slug\":").count(), DiscoTarget::ALL.len());
    assert!(json.contains("\"resolved_jid_retention\": \"forbidden\""));
    assert!(json.contains("\"observed_identity_name_retention\": \"forbidden\""));
    assert!(!json.contains('@'));
}

#[test]
fn every_target_has_an_identity_and_feature_vector() {
    for target in DiscoTarget::ALL {
        assert!(
            !target_identity_contracts(target).is_empty(),
            "{}",
            target.slug()
        );
        let required = required_target_features(target);
        let features = claimable_target_features(target);
        assert!(!required.is_empty(), "{}", target.slug());
        assert!(
            required.iter().all(|feature| features.contains(feature)),
            "{} required features must be claimable",
            target.slug()
        );
        assert!(
            required.contains(&Feature::disco_info()),
            "{} must advertise XEP-0030 support",
            target.slug()
        );
        assert_eq!(
            features.len(),
            features
                .iter()
                .map(|feature| feature.0.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            "{} has duplicate feature namespaces",
            target.slug()
        );

        let identity_names = target_identity_contracts(target)
            .iter()
            .filter_map(|identity| identity.name)
            .collect::<BTreeSet<_>>();
        assert!(
            identity_names.len() <= 1,
            "{} gives multiple XEP-0030 identities different names",
            target.slug()
        );
    }
}

#[test]
fn runtime_dependent_contracts_have_required_baselines_and_complete_unions() {
    let server_variants = runtime_target_feature_variants(DiscoTarget::Server);
    assert_eq!(server_variants.len(), 4);
    let call_features = waddle_xmpp::disco::info::call_features();
    let server_modes = server_variants
        .iter()
        .map(|variant| {
            let advertised_call_features = call_features
                .iter()
                .filter(|feature| variant.contains(feature))
                .count();
            assert!(
                advertised_call_features == 0 || advertised_call_features == call_features.len(),
                "call support must be advertised as one complete feature group"
            );
            (
                advertised_call_features == call_features.len(),
                variant.contains(&Feature::new(waddle_xmpp::isr::ISR_NS)),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(server_modes.len(), 4);

    let server_required = required_target_features(DiscoTarget::Server);
    let server_claimable = claimable_target_features(DiscoTarget::Server);
    let isr = Feature::new(waddle_xmpp::isr::ISR_NS);
    assert!(!server_required.contains(&isr));
    assert!(server_claimable.contains(&isr));

    let room_variants = runtime_target_feature_variants(DiscoTarget::RepresentativeMucRoom);
    assert_eq!(room_variants.len(), 64);
    for variant in &room_variants {
        for pair in [
            [Feature::muc_membersonly(), Feature::muc_open()],
            [Feature::muc_hidden(), Feature::muc_public()],
            [Feature::muc_moderated(), Feature::muc_unmoderated()],
        ] {
            assert_eq!(
                pair.iter()
                    .filter(|feature| variant.contains(feature))
                    .count(),
                1,
                "each room variant must contain exactly one feature from {} / {}",
                pair[0].0,
                pair[1].0,
            );
        }
    }

    let room_required = required_target_features(DiscoTarget::RepresentativeMucRoom);
    let room_claimable = claimable_target_features(DiscoTarget::RepresentativeMucRoom);
    for optional in [
        Feature::muc_persistent(),
        Feature::muc_membersonly(),
        Feature::muc_open(),
        Feature::muc_hidden(),
        Feature::muc_public(),
        Feature::muc_moderated(),
        Feature::muc_unmoderated(),
    ] {
        assert!(!room_required.contains(&optional), "{}", optional.0);
        assert!(room_claimable.contains(&optional), "{}", optional.0);
    }
    assert!(room_required.contains(&Feature::muc()));
}

#[test]
fn curated_extension_union_matches_the_published_deployment_contract() {
    let source = include_str!("../../../../../deployment.cue");
    let published = source
        .split_once("#PublishedExtensionModules:")
        .expect("published extension block")
        .1
        .split_once("#CheckedInGitOpsValues:")
        .expect("end of published extension block")
        .0;
    let names = published
        .lines()
        .filter_map(|line| line.trim().strip_prefix("name:"))
        .map(|value| value.trim().trim_matches('"'));
    let namespaces = published
        .lines()
        .filter_map(|line| line.trim().strip_prefix("namespace:"))
        .map(|value| value.trim().trim_matches('"'));
    assert_eq!(
        names.zip(namespaces).collect::<Vec<_>>(),
        CURATED_EXTENSION_NAMESPACES
    );

    for target in [
        DiscoTarget::Server,
        DiscoTarget::MucService,
        DiscoTarget::ExtensionsService,
        DiscoTarget::RepresentativeMucRoom,
    ] {
        let required = required_target_features(target);
        let claimable = claimable_target_features(target);
        for feature in curated_extension_features() {
            assert!(
                !required.contains(&feature),
                "{}: {}",
                target.slug(),
                feature.0
            );
            assert!(
                claimable.contains(&feature),
                "{}: {}",
                target.slug(),
                feature.0
            );
        }
    }
}

#[test]
fn calls_require_sfu_jingle_and_external_service_discovery() {
    for has_sfu in [false, true] {
        for has_jingle in [false, true] {
            for has_extdisco in [false, true] {
                assert_eq!(
                    calls_available_from_parts(has_sfu, has_jingle, has_extdisco),
                    has_sfu && has_jingle && has_extdisco,
                    "sfu={has_sfu}, jingle={has_jingle}, extdisco={has_extdisco}",
                );
            }
        }
    }
}
