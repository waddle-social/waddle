//! XEP-0490: Message Displayed Synchronization dedicated client suite.

use jid::{BareJid, Jid};
use minidom::Element;
use waddle_xmpp_client::{
    mds::{
        build_mds_publish_iq, parse_mds_catchup_result, parse_mds_event, MDS_NODE, NS_MDS,
        NS_STANZA_ID,
    },
    messaging::NS_CLIENT,
    pep::{NS_PUBSUB, NS_PUBSUB_EVENT},
    StanzaId,
};

fn jid(value: &str) -> Jid {
    value.parse().expect("test JID parses")
}

fn bare_jid(value: &str) -> BareJid {
    value.parse().expect("test bare JID parses")
}

fn stanza_id(value: &str) -> StanzaId {
    StanzaId::new(value).expect("test stanza id is non-empty")
}

fn item(namespace: &str, chat_id: &Jid, stanza_id: &StanzaId, by: &BareJid) -> Element {
    Element::builder("item", namespace)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            chat_id.as_str(),
        )
        .append(
            Element::builder("displayed", NS_MDS)
                .append(
                    Element::builder("stanza-id", NS_STANZA_ID)
                        .attr(
                            minidom::rxml::xml_ncname!("id").to_owned(),
                            stanza_id.as_str(),
                        )
                        .attr(minidom::rxml::xml_ncname!("by").to_owned(), by.as_str())
                        .build(),
                )
                .build(),
        )
        .build()
}

#[test]
fn xep0490_muc_pm_publish_uses_full_occupant_item_id() {
    let occupant = jid("room@conference.example/alice");
    let account_server = bare_jid("example.com");
    let iq = build_mds_publish_iq("mds-1", &occupant, &stanza_id("sid-alice"), &account_server);
    let published = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .and_then(|publish| publish.get_child("item", NS_PUBSUB))
        .expect("published item");

    assert_eq!(published.attr("id"), Some("room@conference.example/alice"));
}

#[test]
fn xep0490_catchup_preserves_two_occupants_in_one_room() {
    let result = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), MDS_NODE)
                        .append(item(
                            NS_PUBSUB,
                            &jid("room@conference.example/alice"),
                            &stanza_id("sid-alice"),
                            &bare_jid("example.com"),
                        ))
                        .append(item(
                            NS_PUBSUB,
                            &jid("room@conference.example/bob"),
                            &stanza_id("sid-bob"),
                            &bare_jid("example.com"),
                        ))
                        .build(),
                )
                .build(),
        )
        .build();

    let entries = parse_mds_catchup_result(&result);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].chat_id, jid("room@conference.example/alice"));
    assert_eq!(entries[1].chat_id, jid("room@conference.example/bob"));
}

#[test]
fn xep0490_cross_resource_event_preserves_full_occupant() {
    let message = Element::builder("message", NS_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "juliet@example.com/phone",
        )
        .append(
            Element::builder("event", NS_PUBSUB_EVENT)
                .append(
                    Element::builder("items", NS_PUBSUB_EVENT)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), MDS_NODE)
                        .append(item(
                            NS_PUBSUB_EVENT,
                            &jid("room@conference.example/alice"),
                            &stanza_id("sid-alice"),
                            &bare_jid("example.com"),
                        ))
                        .build(),
                )
                .build(),
        )
        .build();

    let entries = parse_mds_event(&message).expect("MDS event");
    assert_eq!(entries[0].chat_id, jid("room@conference.example/alice"));
}

#[test]
fn xep0490_rejects_resource_bearing_stanza_id_authority() {
    let message = Element::builder("message", NS_CLIENT)
        .append(
            Element::builder("event", NS_PUBSUB_EVENT)
                .append(
                    Element::builder("items", NS_PUBSUB_EVENT)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), MDS_NODE)
                        .append(
                            Element::builder("item", NS_PUBSUB_EVENT)
                                .attr(
                                    minidom::rxml::xml_ncname!("id").to_owned(),
                                    "room@conference.example/alice",
                                )
                                .append(
                                    Element::builder("displayed", NS_MDS)
                                        .append(
                                            Element::builder("stanza-id", NS_STANZA_ID)
                                                .attr(
                                                    minidom::rxml::xml_ncname!("id").to_owned(),
                                                    "sid-alice",
                                                )
                                                .attr(
                                                    minidom::rxml::xml_ncname!("by").to_owned(),
                                                    "example.com/phone",
                                                )
                                                .build(),
                                        )
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    assert_eq!(parse_mds_event(&message), Some(Vec::new()));
}
