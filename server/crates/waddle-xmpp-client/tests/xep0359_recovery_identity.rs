//! XEP-0359: stable recovery identity for stream-management recovery.
//!
//! XEP-0198 resumption replays the persisted unhandled tail, while a failed
//! resumption retries it on a fresh stream with XEP-0203 delayed-delivery
//! metadata. Both recovery paths must retain the typed XEP-0359 identity.

use chrono::{DateTime, TimeZone, Utc};
use minidom::Element;
use waddle_xmpp_client::{
    stream_management::SmState, SmResumeState, StreamId, UnhandledOutboundEntry,
};
use waddle_xmpp_core::xep0359::{
    add_origin_id, add_stanza_id, extract_origin_id, extract_stanza_ids, OriginId,
    StanzaId as StableStanzaId,
};
use xmpp_parsers::message::{Lang, Message};

const NS_DELAY: &str = "urn:xmpp:delay";

fn typed_message_with_xep0359_ids(
    origin_id: &OriginId,
    stable_stanza_id: &StableStanzaId,
) -> Element {
    let recipient = "room@muc.example".parse().expect("valid room JID");
    let mut message = Message::groupchat(Some(recipient));
    message
        .bodies
        .insert(Lang::new(), "Persisted recovery identity".to_owned());
    add_origin_id(&mut message, origin_id.as_str());
    add_stanza_id(&mut message, stable_stanza_id);
    Element::from(message)
}

fn resume_state_for(stanza: Element) -> SmResumeState {
    let sent_at = Utc
        .with_ymd_and_hms(2026, 7, 28, 10, 11, 12)
        .single()
        .expect("test timestamp is valid");
    SmResumeState::from_unhandled_outbound_entries(
        StreamId::new("previous-stream"),
        0,
        1,
        [UnhandledOutboundEntry::try_new(stanza, sent_at)
            .expect("typed message is countable for XEP-0198")],
    )
    .expect("XEP-0198 resume state is valid")
}

fn assert_xep0359_identity(
    stanza: &Element,
    expected_origin_id: &OriginId,
    expected_stanza_id: &StableStanzaId,
) {
    let parsed = Message::try_from(stanza.clone()).expect("recovered stanza parses as a message");
    assert_eq!(
        extract_origin_id(&parsed),
        Some(expected_origin_id.clone()),
        "XEP-0359 origin-id remains the original typed identity",
    );
    assert_eq!(
        extract_stanza_ids(&parsed),
        vec![expected_stanza_id.clone()],
        "the existing retry implementation preserves the server-assigned stanza-id",
    );
}

#[test]
fn xep0359_successful_sm_resume_replay_retains_origin_identity() {
    let origin_id = OriginId::new("origin-recovery-1");
    let stable_stanza_id = StableStanzaId::new(
        "archive-recovery-1",
        "room@muc.example"
            .parse()
            .expect("valid stanza-id authority"),
    );
    let resume_state = resume_state_for(typed_message_with_xep0359_ids(
        &origin_id,
        &stable_stanza_id,
    ));

    let mut sm = SmState::from_resume_state(&resume_state);
    let replay = sm.mark_unhandled_for_replay();

    assert_eq!(
        replay.len(),
        1,
        "one unhandled message is replayed after <resumed/>"
    );
    assert_xep0359_identity(&replay[0], &origin_id, &stable_stanza_id);
    assert!(
        replay[0].get_child("delay", NS_DELAY).is_none(),
        "successful XEP-0198 replay is not a fresh-stream delayed retry",
    );
}

#[test]
fn xep0359_failed_resume_fallback_retry_retains_identity_and_one_delay() {
    let origin_id = OriginId::new("origin-recovery-2");
    let stable_stanza_id = StableStanzaId::new(
        "archive-recovery-2",
        "room@muc.example"
            .parse()
            .expect("valid stanza-id authority"),
    );
    let resume_state = resume_state_for(typed_message_with_xep0359_ids(
        &origin_id,
        &stable_stanza_id,
    ));

    let sm = SmState::from_resume_state(&resume_state);
    let retries = sm.unhandled_stanzas_for_fallback_retry();

    assert_eq!(
        retries.len(),
        1,
        "one unhandled message is retried after <failed/>"
    );
    let retry = &retries[0];
    assert_xep0359_identity(retry, &origin_id, &stable_stanza_id);

    let delays = retry
        .children()
        .filter(|child| child.name() == "delay" && child.ns() == NS_DELAY)
        .collect::<Vec<_>>();
    assert_eq!(
        delays.len(),
        1,
        "XEP-0203 fallback retry has exactly one direct-child delay",
    );
    let stamp = delays[0].attr("stamp").expect("XEP-0203 delay stamp");
    assert_eq!(
        DateTime::parse_from_rfc3339(stamp)
            .expect("XEP-0203 delay stamp is RFC 3339")
            .with_timezone(&Utc),
        Utc.with_ymd_and_hms(2026, 7, 28, 10, 11, 12)
            .single()
            .expect("test timestamp is valid"),
        "XEP-0203 records the original delivery instant",
    );
    assert_eq!(
        delays[0].children().count(),
        0,
        "XEP-0203 delay has no child payload",
    );
}
