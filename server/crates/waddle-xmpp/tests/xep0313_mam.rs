use waddle_xmpp::disco::info::{muc_room_features, server_features, Feature};

#[test]
fn server_root_disco_does_not_advertise_a_domain_archive() {
    let features = server_features();

    assert!(!features.contains(&Feature::mam()));
    assert!(!features.contains(&Feature::mam_extended()));
}

#[test]
fn muc_room_disco_advertises_mam_extended_for_supported_id_filters() {
    let features = muc_room_features(true, true, true, false, false);

    assert!(features.contains(&Feature::mam()));
    assert!(features.contains(&Feature::mam_extended()));
}
