use super::{
    build_activity_element_with_specific, build_mood_clear_element, build_mood_element,
    build_pep_items_iq, build_pep_publish_iq, build_pep_publish_iq_with_item_id,
    build_publish_mood_iq, build_publish_tune_iq, build_retract_activity_iq, build_tune_element,
    parse_pep_activity, parse_pep_mood, parse_pep_tune, Element, UserActivity, UserMood, UserTune,
    NS_ACTIVITY, NS_CLIENT, NS_MOOD, NS_PUBSUB, NS_PUBSUB_EVENT, NS_TUNE,
};

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
    let iq = build_publish_mood_iq("mood-req-1", "content", Some("steady"));
    assert_eq!(iq.attr("id"), Some("mood-req-1"));
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
    let iq = build_retract_activity_iq("activity-req-1");
    assert_eq!(iq.attr("id"), Some("activity-req-1"));
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
        "tune-req-1",
        &UserTune {
            artist: Some("The Beatles".to_string()),
            title: Some("Come Together".to_string()),
            source: Some("Abbey Road".to_string()),
            length: Some(259),
            rating: Some(9),
            track: Some("1".to_string()),
            uri: Some("https://example.com/track".to_string()),
        },
    );
    assert_eq!(iq.attr("id"), Some("tune-req-1"));
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
    let iq = build_pep_items_iq("items-req-1", "alice@example.com", NS_MOOD);
    assert_eq!(iq.attr("id"), Some("items-req-1"));
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

fn activity_event(from: &str, kind: Option<&str>, child: Element) -> Element {
    let mut message = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
        .append(
            Element::builder("event", NS_PUBSUB_EVENT)
                .append(child)
                .build(),
        );
    if let Some(kind) = kind {
        message = message.attr(minidom::rxml::xml_ncname!("type").to_owned(), kind);
    }
    message.build()
}

fn activity_node(name: &str, children: impl IntoIterator<Item = Element>) -> Element {
    Element::builder(name, NS_PUBSUB_EVENT)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_ACTIVITY)
        .append_all(children)
        .build()
}

fn activity_item(payload: Element) -> Element {
    Element::builder("item", NS_PUBSUB_EVENT)
        .append(payload)
        .build()
}

fn notified(message: &Element) -> Option<(String, Option<UserActivity>)> {
    let account = "alice@example.com".parse().unwrap();
    super::parse_activity_notification(message, &account)
        .map(|(from, activity)| (from.to_string(), activity))
}

fn on_the_phone() -> UserActivity {
    UserActivity {
        activity: "talking".to_string(),
        specific: Some("on_the_phone".to_string()),
        text: None,
    }
}

fn on_the_phone_payload() -> Element {
    build_activity_element_with_specific("talking", Some("on_the_phone"), None)
}

#[test]
fn activity_notification_names_the_publisher_and_the_activity() {
    let items = activity_node("items", [activity_item(on_the_phone_payload())]);
    assert_eq!(
        notified(&activity_event("bob@example.com", Some("headline"), items)),
        Some(("bob@example.com".to_string(), Some(on_the_phone())))
    );
}

#[test]
fn activity_notification_accepts_the_extensions_of_xep_0108() {
    // XEP-0108 §5: a specific activity, or a detail under one, may come from
    // another namespace; a receiver that does not know it ignores it.
    let foreign_specific: Element =
        "<activity xmlns='http://jabber.org/protocol/activity'><relaxing><tanning xmlns='http://www.ilovetanning.info'/></relaxing></activity>"
            .parse()
            .unwrap();
    let foreign_detail: Element =
        "<activity xmlns='http://jabber.org/protocol/activity'><inactive><sleeping><hibernating xmlns='http://www.ursus.info/states'/></sleeping></inactive><text xml:lang='en'>zzz</text><text xml:lang='de'>zzz</text></activity>"
            .parse()
            .unwrap();
    let relaxing = notified(&activity_event(
        "bob@example.com",
        None,
        activity_node("items", [activity_item(foreign_specific)]),
    ));
    assert_eq!(
        relaxing.unwrap().1,
        Some(UserActivity {
            activity: "relaxing".to_string(),
            specific: None,
            text: None,
        })
    );
    let sleeping = notified(&activity_event(
        "bob@example.com",
        None,
        activity_node("items", [activity_item(foreign_detail)]),
    ));
    assert_eq!(
        sleeping.unwrap().1,
        Some(UserActivity {
            activity: "inactive".to_string(),
            specific: Some("sleeping".to_string()),
            text: Some("zzz".to_string()),
        })
    );
}

#[test]
fn the_last_change_listed_is_the_activity_now() {
    let retract = Element::builder("retract", NS_PUBSUB_EVENT).build();
    let published_then_retracted = activity_node(
        "items",
        [activity_item(on_the_phone_payload()), retract.clone()],
    );
    let retracted_then_published =
        activity_node("items", [retract, activity_item(on_the_phone_payload())]);
    assert_eq!(
        notified(&activity_event(
            "bob@example.com",
            None,
            published_then_retracted
        ))
        .unwrap()
        .1,
        None
    );
    assert_eq!(
        notified(&activity_event(
            "bob@example.com",
            None,
            retracted_then_published
        ))
        .unwrap()
        .1,
        Some(on_the_phone())
    );
}

#[test]
fn a_deleted_or_purged_activity_node_holds_no_activity() {
    for name in ["delete", "purge"] {
        assert_eq!(
            notified(&activity_event(
                "bob@example.com",
                None,
                activity_node(name, [])
            )),
            Some(("bob@example.com".to_string(), None)),
            "{name}"
        );
    }
}

#[test]
fn activity_notification_is_not_believed_from_the_wrong_sender() {
    let items = || activity_node("items", [activity_item(on_the_phone_payload())]);
    // Our own account, a device, a MUC occupant, and a room's groupchat.
    for (from, kind) in [
        ("alice@example.com", None),
        ("bob@example.com/phone", None),
        ("room@muc.example.com/bob", None),
        ("room@muc.example.com", Some("groupchat")),
    ] {
        assert_eq!(
            notified(&activity_event(from, kind, items())),
            None,
            "{from}"
        );
    }
}

#[test]
fn a_change_to_another_node_is_not_an_activity_notification() {
    for name in ["items", "delete", "purge"] {
        let other = Element::builder(name, NS_PUBSUB_EVENT)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_MOOD)
            .build();
        assert_eq!(
            notified(&activity_event("bob@example.com", None, other)),
            None
        );
    }
    let unknown_change = activity_node(
        "items",
        [Element::builder("other", NS_PUBSUB_EVENT).build()],
    );
    assert_eq!(
        notified(&activity_event("bob@example.com", None, unknown_change)),
        None
    );
}
