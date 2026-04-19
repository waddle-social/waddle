//! XEP-0107: User Mood — dedicated suite.

use minidom::Element;
use waddle_xmpp::pubsub::{AccessModel, PepHandler};
use waddle_xmpp::xep::xep0107::{
    build_mood_element, build_mood_retraction, is_mood_element, parse_mood_element, Mood,
    MoodError, MoodKind, NS_MOOD,
};

const MOOD_NODE: &str = "http://jabber.org/protocol/mood";

#[test]
fn xep0107_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(MOOD_NODE));
}

#[test]
fn xep0107_access_model_is_presence() {
    assert_eq!(
        PepHandler::default_access_model_for_node(MOOD_NODE),
        AccessModel::Presence
    );
}

#[test]
fn xep0107_round_trip_with_text() {
    let m = Mood::new(MoodKind::Happy).with_text("Shipping MIX");
    let elem = build_mood_element(&m);
    let parsed = parse_mood_element(&elem).unwrap().unwrap();
    assert_eq!(parsed, m);
}

#[test]
fn xep0107_retraction_is_empty_mood_element() {
    let retraction = build_mood_retraction();
    assert!(is_mood_element(&retraction));
    assert!(parse_mood_element(&retraction).unwrap().is_none());
}

#[test]
fn xep0107_unknown_kind_rejected() {
    let elem = Element::builder("mood", NS_MOOD)
        .append(Element::builder("euphoric", NS_MOOD).build())
        .build();
    assert!(matches!(
        parse_mood_element(&elem),
        Err(MoodError::UnknownKind(_))
    ));
}

#[test]
fn xep0107_wrong_namespace_rejected() {
    let elem = Element::builder("mood", "other").build();
    assert_eq!(parse_mood_element(&elem), Err(MoodError::WrongElement));
}

#[test]
fn xep0107_all_kinds_round_trip_element_names() {
    let kinds = [
        MoodKind::Happy,
        MoodKind::Sad,
        MoodKind::InLove,
        MoodKind::InAwe,
        MoodKind::Contemplative,
        MoodKind::Undefined,
    ];
    for k in kinds {
        assert_eq!(MoodKind::from_element_name(k.as_element_name()), Some(k));
    }
}
