use super::*;

fn make_pep_message(node: &str, payload: Element) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "user@example.com",
        )
        .append(
            Element::builder("event", NS_PUBSUB_EVENT)
                .append(
                    Element::builder("items", NS_PUBSUB_EVENT)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                        .append(
                            Element::builder("item", NS_PUBSUB_EVENT)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "current")
                                .append(payload)
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

#[test]
fn parse_mood_happy_path() {
    let msg = make_pep_message(NS_MOOD, build_mood_element("happy", Some("feeling good")));
    assert_eq!(
        parse(&msg),
        Some(PepItem::Mood(UserMood {
            mood: "happy".to_string(),
            text: Some("feeling good".to_string()),
        }))
    );
}

#[test]
fn parse_mood_cleared() {
    let msg = make_pep_message(NS_MOOD, build_mood_clear_element());
    assert_eq!(parse(&msg), Some(PepItem::MoodCleared));
}

#[test]
fn parse_activity_with_specific_and_text() {
    let msg = make_pep_message(
        NS_ACTIVITY,
        build_activity_element_with_specific("working", Some("coding"), Some("rewriting waddle")),
    );
    assert_eq!(
        parse(&msg),
        Some(PepItem::Activity(UserActivity {
            activity: "working".to_string(),
            specific: Some("coding".to_string()),
            text: Some("rewriting waddle".to_string()),
        }))
    );
}

#[test]
fn parse_tune_full() {
    let msg = make_pep_message(
        NS_TUNE,
        build_tune_element(&UserTune {
            artist: Some("Artist Name".to_string()),
            title: Some("Song Title".to_string()),
            source: Some("Album".to_string()),
            length: Some(213),
            rating: Some(9),
            track: Some("7".to_string()),
            uri: Some("https://example.com/song".to_string()),
        }),
    );
    assert_eq!(
        parse(&msg),
        Some(PepItem::Tune(UserTune {
            artist: Some("Artist Name".to_string()),
            title: Some("Song Title".to_string()),
            source: Some("Album".to_string()),
            length: Some(213),
            rating: Some(9),
            track: Some("7".to_string()),
            uri: Some("https://example.com/song".to_string()),
        }))
    );
}

#[test]
fn parse_tune_cleared() {
    let msg = make_pep_message(NS_TUNE, build_tune_clear_element());
    assert_eq!(parse(&msg), Some(PepItem::TuneCleared));
}

#[test]
fn build_pep_publish_iq_uses_current_item_id() {
    let iq = build_pep_publish_iq("iq-1", NS_MOOD, build_mood_clear_element());
    let item = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .and_then(|publish| publish.get_child("item", NS_PUBSUB))
        .expect("item");
    assert_eq!(item.attr("id"), Some("current"));
}

#[test]
fn build_pep_publish_iq_with_item_id_uses_custom_item_id() {
    let iq = build_pep_publish_iq_with_item_id(
        "iq-2",
        "urn:xmpp:avatar:data",
        "cafef00d",
        Element::builder("data", "urn:xmpp:avatar:data").build(),
    );
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("id"), Some("iq-2"));
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish");
    assert_eq!(publish.attr("node"), Some("urn:xmpp:avatar:data"));
    let item = publish.get_child("item", NS_PUBSUB).expect("item");
    assert_eq!(item.attr("id"), Some("cafef00d"));
}

#[test]
fn build_publish_mood_iq_structure() {
    let iq = build_publish_mood_iq("content", Some("steady"));
    let mood = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .and_then(|publish| publish.get_child("item", NS_PUBSUB))
        .and_then(|item| item.get_child("mood", NS_MOOD))
        .expect("mood payload");
    assert!(mood.get_child("content", NS_MOOD).is_some());
    assert_eq!(
        mood.get_child("text", NS_MOOD).map(|child| child.text()),
        Some("steady".to_string())
    );
}

#[test]
fn build_retract_activity_iq_publishes_empty_activity() {
    let iq = build_retract_activity_iq();
    let activity = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .and_then(|publish| publish.get_child("item", NS_PUBSUB))
        .and_then(|item| item.get_child("activity", NS_ACTIVITY))
        .expect("activity payload");
    assert!(activity.children().next().is_none());
}

#[test]
fn build_publish_tune_iq_includes_extended_fields() {
    let iq = build_publish_tune_iq(
        Some("The Beatles"),
        Some("Come Together"),
        Some("Abbey Road"),
        Some(259),
        Some(9),
        Some("1"),
        Some("https://example.com/track"),
    );
    let tune = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .and_then(|publish| publish.get_child("item", NS_PUBSUB))
        .and_then(|item| item.get_child("tune", NS_TUNE))
        .expect("tune payload");
    assert_eq!(
        tune.get_child("rating", NS_TUNE).map(|child| child.text()),
        Some("9".to_string())
    );
    assert_eq!(
        tune.get_child("track", NS_TUNE).map(|child| child.text()),
        Some("1".to_string())
    );
}

#[test]
fn build_pep_items_iq_targets_requested_jid_and_node() {
    let iq = build_pep_items_iq("alice@example.com", NS_MOOD);
    assert_eq!(iq.attr("to"), Some("alice@example.com"));
    assert_eq!(
        iq.get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .and_then(|items| items.attr("node")),
        Some(NS_MOOD)
    );
}

#[test]
fn parse_pep_iq_results_round_trip() {
    let mood_iq = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_MOOD)
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "current")
                                .append(build_mood_element("happy", Some("great")))
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    assert_eq!(
        parse_pep_mood(&mood_iq),
        Some(UserMood {
            mood: "happy".to_string(),
            text: Some("great".to_string()),
        })
    );

    let activity_iq = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_ACTIVITY)
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "current")
                                .append(build_activity_element_with_specific(
                                    "working",
                                    Some("coding"),
                                    Some("flow"),
                                ))
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    assert_eq!(
        parse_pep_activity(&activity_iq),
        Some(UserActivity {
            activity: "working".to_string(),
            specific: Some("coding".to_string()),
            text: Some("flow".to_string()),
        })
    );

    let tune_iq = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_TUNE)
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "current")
                                .append(build_tune_element(&UserTune {
                                    artist: Some("Artist".to_string()),
                                    title: Some("Track".to_string()),
                                    source: Some("Album".to_string()),
                                    length: Some(180),
                                    rating: Some(8),
                                    track: Some("4".to_string()),
                                    uri: Some("https://example.com/tune".to_string()),
                                }))
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    assert_eq!(
        parse_pep_tune(&tune_iq),
        Some(UserTune {
            artist: Some("Artist".to_string()),
            title: Some("Track".to_string()),
            source: Some("Album".to_string()),
            length: Some(180),
            rating: Some(8),
            track: Some("4".to_string()),
            uri: Some("https://example.com/tune".to_string()),
        })
    );
}

#[test]
fn parse_ignores_non_pep_message() {
    let msg = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .append(Element::builder("body", NS_CLIENT).append("Hello!").build())
        .build();
    assert_eq!(parse(&msg), None);
}

#[test]
fn parse_ignores_iq() {
    let iq = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "some-id")
        .build();
    assert_eq!(parse(&iq), None);
}
