//! XEP-0118: User Tune — dedicated suite.

use minidom::Element;
use waddle_xmpp::pubsub::{AccessModel, PepHandler};
use waddle_xmpp::xep::xep0118::{
    build_tune_element, build_tune_retraction, is_tune_element, parse_tune_element, Tune,
    TuneError, NS_TUNE,
};

const TUNE_NODE: &str = "http://jabber.org/protocol/tune";

#[test]
fn xep0118_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(TUNE_NODE));
}

#[test]
fn xep0118_access_model_is_presence() {
    assert_eq!(
        PepHandler::default_access_model_for_node(TUNE_NODE),
        AccessModel::Presence
    );
}

#[test]
fn xep0118_full_round_trip() {
    let tune = Tune::new()
        .with_artist("Daft Punk")
        .with_title("Around the World")
        .with_source("Homework")
        .with_track("1")
        .with_length(422)
        .with_rating(9)
        .with_uri("https://example.com/track");
    let elem = build_tune_element(&tune);
    let parsed = parse_tune_element(&elem).unwrap();
    assert_eq!(parsed, tune);
}

#[test]
fn xep0118_retraction_is_empty_tune_element() {
    let elem = build_tune_retraction();
    assert!(is_tune_element(&elem));
    let parsed = parse_tune_element(&elem).unwrap();
    assert!(parsed.is_empty());
}

#[test]
fn xep0118_rating_out_of_range_rejected() {
    let elem = Element::builder("tune", NS_TUNE)
        .append(Element::builder("rating", NS_TUNE).append("0").build())
        .build();
    assert!(matches!(
        parse_tune_element(&elem),
        Err(TuneError::InvalidRating(_))
    ));
    let elem = Element::builder("tune", NS_TUNE)
        .append(Element::builder("rating", NS_TUNE).append("11").build())
        .build();
    assert!(matches!(
        parse_tune_element(&elem),
        Err(TuneError::InvalidRating(_))
    ));
}

#[test]
fn xep0118_length_must_be_u32() {
    let elem = Element::builder("tune", NS_TUNE)
        .append(
            Element::builder("length", NS_TUNE)
                .append("not-a-number")
                .build(),
        )
        .build();
    assert!(matches!(
        parse_tune_element(&elem),
        Err(TuneError::InvalidLength(_))
    ));
}

#[test]
fn xep0118_wrong_namespace_rejected() {
    let elem = Element::builder("tune", "other").build();
    assert_eq!(parse_tune_element(&elem), Err(TuneError::WrongElement));
}
