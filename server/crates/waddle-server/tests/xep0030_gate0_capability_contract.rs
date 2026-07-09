//! XEP-0030 Gate 0 capability-contract conformance.
//!
//! XEP-0030 section 3 requires every successful disco#info result to contain
//! at least one identity and the disco#info feature. Gate evidence additionally
//! needs a target-local union for legitimate deployment and room modes.

use waddle_server::server::disco_targets::{
    claimable_target_features, required_target_features, runtime_target_feature_variants,
    target_identity_contracts, DiscoTarget,
};
use waddle_xmpp::disco::Feature;

#[test]
fn every_target_requires_disco_info_and_keeps_required_features_claimable() {
    for target in DiscoTarget::ALL {
        assert!(
            !target_identity_contracts(target).is_empty(),
            "{}",
            target.slug()
        );
        let required = required_target_features(target);
        let claimable = claimable_target_features(target);
        assert!(
            required.contains(&Feature::disco_info()),
            "{}",
            target.slug()
        );
        assert!(
            required.iter().all(|feature| claimable.contains(feature)),
            "{}",
            target.slug()
        );
    }
}

#[test]
fn runtime_union_covers_clustered_isr_and_non_default_muc_room_modes() {
    let server = claimable_target_features(DiscoTarget::Server);
    assert!(server.contains(&Feature::new(waddle_xmpp::isr::ISR_NS)));

    let room = claimable_target_features(DiscoTarget::RepresentativeMucRoom);
    for feature in [
        Feature::muc_persistent(),
        Feature::muc_membersonly(),
        Feature::muc_open(),
        Feature::muc_hidden(),
        Feature::muc_public(),
        Feature::muc_moderated(),
        Feature::muc_unmoderated(),
    ] {
        assert!(room.contains(&feature), "{}", feature.0);
    }

    for target in [
        DiscoTarget::Server,
        DiscoTarget::MucService,
        DiscoTarget::ExtensionsService,
        DiscoTarget::RepresentativeMucRoom,
    ] {
        let claimable = claimable_target_features(target);
        for namespace in [
            "urn:waddle:link-board:1",
            "urn:waddle:ai-chatbot:1",
            "urn:waddle:decision-polls:1",
            "urn:waddle:web-integration:1",
            "urn:waddle:stargate-quotes:1",
        ] {
            assert!(
                claimable.contains(&Feature::new(namespace)),
                "{target:?}: {namespace}"
            );
        }
    }
}

#[test]
fn runtime_vectors_are_complete_server_and_muc_configurations() {
    let server_variants = runtime_target_feature_variants(DiscoTarget::Server);
    assert_eq!(server_variants.len(), 4);
    let call_features = waddle_xmpp::disco::info::call_features();
    for variant in &server_variants {
        let call_feature_count = call_features
            .iter()
            .filter(|feature| variant.contains(feature))
            .count();
        assert!(
            call_feature_count == 0 || call_feature_count == call_features.len(),
            "a server cannot claim only part of its call surface"
        );
    }

    let room_variants = runtime_target_feature_variants(DiscoTarget::RepresentativeMucRoom);
    assert_eq!(room_variants.len(), 64);
    for variant in &room_variants {
        for exclusive_pair in [
            [Feature::muc_membersonly(), Feature::muc_open()],
            [Feature::muc_hidden(), Feature::muc_public()],
            [Feature::muc_moderated(), Feature::muc_unmoderated()],
        ] {
            assert_eq!(
                exclusive_pair
                    .iter()
                    .filter(|feature| variant.contains(feature))
                    .count(),
                1,
                "room configuration features must remain mutually exclusive"
            );
        }
    }
}
