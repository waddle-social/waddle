//! XEP-0198 + XEP-0203 — delay stamping on `<resumed/>` replay
//! (issue #1178).
//!
//! When a stream resumes, unacked stanzas replayed from the queue MUST
//! carry a `<delay xmlns='urn:xmpp:delay'/>` whose `stamp` is the
//! server-side receipt time of the original stanza — not the replay
//! time — so clients sort them at their true timeline position instead
//! of the drain time. XEP-0198's Acks section requires this delay for
//! failed-session redelivery ("add a delay element with the original
//! (failed) delivery timestamp, as per XEP-0203"); we apply the same
//! stamping to the `<resumed/>` replay by analogy.

use chrono::{DateTime, TimeZone, Utc};
use jid::Jid;
use minidom::Element;
use waddle_xmpp::parser::element_to_string;
use waddle_xmpp::stream_management::persistence::SmUnackedStanzaPurpose;
use waddle_xmpp::stream_management::{stamp_replay_delay, StreamManagementState};
use waddle_xmpp::xep::xep0203::{build_delay_element, DelayInfo};
use waddle_xmpp::xep::NS_DELAY;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

fn fixed_receipt() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 1, 9, 15, 30).unwrap()
}

/// Build a typed chat `<message/>` fixture (bob → alice) with the given
/// id, body, and any pre-attached `<delay/>` payloads.
fn chat_message(id: &str, body: &str, delays: Vec<Element>) -> Stanza {
    let mut message = Message::new(Some("alice@example.com".parse::<Jid>().expect("jid")));
    message.from = Some("bob@example.com/x".parse::<Jid>().expect("jid"));
    message.type_ = MessageType::Chat;
    message.id = Some(Id(id.to_string()));
    message.bodies.insert(Lang::new(), body.to_string());
    message.payloads.extend(delays);
    Stanza::Message(message)
}

fn stanza_to_xml(stanza: &Stanza) -> String {
    element_to_string(&stanza.to_element()).expect("serialize stanza fixture")
}

fn delay_children(stanza: &Stanza) -> Vec<Element> {
    stanza
        .to_element()
        .children()
        .filter(|child| child.name() == "delay" && child.ns() == NS_DELAY)
        .cloned()
        .collect()
}

fn iq_error(id: &str, payload: Option<Element>) -> Stanza {
    Stanza::Iq(Box::new(Iq::Error {
        from: Some("example.com".parse().expect("from JID")),
        to: Some("alice@example.com/r".parse().expect("to JID")),
        id: id.to_owned(),
        error: StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "service unavailable",
        ),
        payload,
    }))
}

fn replay_one(stanza: Stanza) -> Stanza {
    let mut state = StreamManagementState::new();
    state.enable("stream-iq-error".to_owned(), true, Some(300));
    let _ = state.record_outbound_with_receipt_at(
        stanza,
        fixed_receipt(),
        SmUnackedStanzaPurpose::Application,
    );

    let mut replay = state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 1, "the unacked IQ must be replayed");
    replay.remove(0).stanza
}

#[test]
fn replayed_message_carries_delay_with_original_receipt_stamp() {
    let original = chat_message("m1", "hi", Vec::new());

    let stamped = stamp_replay_delay(&original, "example.com", fixed_receipt());

    let delays = delay_children(&stamped);
    assert_eq!(delays.len(), 1, "exactly one delay element on replay");
    assert_eq!(delays[0].attr("from"), Some("example.com"));
    // XEP-0082 §3.2 canonical UTC form with the `Z` suffix, matching
    // the original receipt time — never the replay time.
    assert_eq!(delays[0].attr("stamp"), Some("2026-07-01T09:15:30Z"));
    assert!(
        delays[0].text().is_empty(),
        "SM resume replay carries no offline-storage reason text"
    );
}

#[test]
fn replayed_iq_is_not_stamped() {
    // XEP-0203 §2 defines delay annotations for message and presence
    // stanzas; a replayed <iq/> must go out byte-identical.
    let original = Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::Result {
        from: Some("example.com".parse().expect("from JID")),
        to: Some("alice@example.com/r".parse().expect("to JID")),
        id: "q1".to_string(),
        payload: None,
    }));

    let stamped = stamp_replay_delay(&original, "example.com", fixed_receipt());

    assert_eq!(
        stamped.to_element(),
        original.to_element(),
        "iq replay is unchanged"
    );
    assert!(delay_children(&stamped).is_empty(), "iq gains no delay");
}

#[test]
fn replay_serialization_preserves_payload_bearing_iq_error() {
    let payload = Element::builder("ping", "urn:xmpp:ping").build();
    let replayed = replay_one(iq_error("payload-error", Some(payload.clone())));

    let Stanza::Iq(iq) = &replayed else {
        panic!("the resend queue must preserve the IQ stanza variant")
    };
    let Iq::Error {
        id,
        payload: replayed_payload,
        error,
        ..
    } = iq.as_ref()
    else {
        panic!("the resend queue must preserve the IQ error variant")
    };
    assert_eq!(id, "payload-error");
    assert_eq!(replayed_payload.as_ref(), Some(&payload));
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );

    let xml = stanza_to_xml(&replayed);
    let element: Element = xml.parse().expect("serialized replayed IQ parses");
    let child_names: Vec<&str> = element.children().map(Element::name).collect();
    assert_eq!(
        child_names,
        vec!["ping", "error"],
        "RFC 6120 IQ errors echo the request payload before <error/>"
    );
}

#[test]
fn replay_serialization_preserves_payloadless_iq_error() {
    let replayed = replay_one(iq_error("payloadless-error", None));

    let Stanza::Iq(iq) = &replayed else {
        panic!("the resend queue must preserve the IQ stanza variant")
    };
    let Iq::Error { id, payload, .. } = iq.as_ref() else {
        panic!("the resend queue must preserve the IQ error variant")
    };
    assert_eq!(id, "payloadless-error");
    assert!(payload.is_none());

    let xml = stanza_to_xml(&replayed);
    let element: Element = xml.parse().expect("serialized replayed IQ parses");
    let child_names: Vec<&str> = element.children().map(Element::name).collect();
    assert_eq!(
        child_names,
        vec!["error"],
        "payload-less IQ errors serialize with only their error child"
    );
}

#[test]
fn replay_preserves_existing_self_stamped_delay_and_upstream_delay() {
    // A queued stanza can already carry a delay this server itself
    // stamped on an earlier path — the offline flush, or a Q6
    // SM-expiry redelivery whose queue receipt time is the REDELIVERY
    // time, not the original send time. In that case the existing
    // self-stamp is the only accurate record of the original time and
    // MUST be kept as-is (no second self-stamp, no overwrite with the
    // later queue receipt). Delays from other entities are the
    // recipient's delivery history and stay untouched.
    let self_stamp = build_delay_element(&DelayInfo {
        from: Some("example.com".to_string()),
        stamp: Utc.with_ymd_and_hms(2026, 6, 30, 8, 0, 0).unwrap(),
        reason: Some("Offline Storage".to_string()),
    });
    let upstream_delay = build_delay_element(&DelayInfo {
        from: Some("upstream.example.org".to_string()),
        stamp: Utc.with_ymd_and_hms(2026, 6, 30, 7, 59, 0).unwrap(),
        reason: Some("Forwarded by upstream".to_string()),
    });
    let original = chat_message("m2", "hi", vec![self_stamp, upstream_delay]);

    let stamped = stamp_replay_delay(&original, "example.com", fixed_receipt());

    let delays = delay_children(&stamped);
    let self_stamped: Vec<_> = delays
        .iter()
        .filter(|delay| delay.attr("from") == Some("example.com"))
        .collect();
    assert_eq!(self_stamped.len(), 1, "exactly one self-stamped delay");
    assert_eq!(
        self_stamped[0].attr("stamp"),
        Some("2026-06-30T08:00:00Z"),
        "the earlier accurate self-stamp is kept, not overwritten with \
         the (possibly later) queue receipt time"
    );
    assert_eq!(
        self_stamped[0].text(),
        "Offline Storage",
        "the original delay's reason text survives replay"
    );
    assert!(
        delays
            .iter()
            .any(|delay| delay.attr("from") == Some("upstream.example.org")),
        "upstream delay metadata preserved"
    );
}

#[test]
fn first_send_is_recorded_unstamped_and_gains_delay_only_at_replay() {
    // The unacked queue must hold the original typed stanza without a
    // replay delay — the XEP-0203 stamp is applied only
    // when the stanza is replayed after <resumed/>. Stamping at record
    // time would leak a delay element onto the FIRST delivery, telling
    // the recipient a live message was delayed.
    let mut state = StreamManagementState::new();
    state.enable("stream-first-send".to_string(), true, Some(300));

    let wire = chat_message("live-1", "live", Vec::new());
    let _ = state.record_outbound_with_receipt_at(
        wire.clone(),
        fixed_receipt(),
        SmUnackedStanzaPurpose::Application,
    );

    let replay = state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 1);
    assert_eq!(
        stanza_to_xml(&replay[0].stanza),
        stanza_to_xml(&wire),
        "queue preserves the original stanza — no delay on first delivery"
    );
    assert!(delay_children(&replay[0].stanza).is_empty());

    let stamped = stamp_replay_delay(
        &replay[0].stanza,
        "example.com",
        replay[0].original_receipt_at,
    );
    assert_eq!(delay_children(&stamped).len(), 1, "delay appears at replay");
}

#[test]
fn replay_delay_preserves_typed_message_thread() {
    let mut original = chat_message("threaded", "reply", Vec::new());
    let Stanza::Message(message) = &mut original else {
        unreachable!("chat_message always returns a message")
    };
    message.thread = Some(xmpp_parsers::message::Thread {
        id: "conversation-thread".to_string(),
        parent: None,
    });

    let stamped = stamp_replay_delay(&original, "example.com", fixed_receipt());

    let Stanza::Message(message) = stamped else {
        panic!("replay delay must preserve the message stanza variant")
    };
    assert_eq!(
        message.thread.as_ref().map(|thread| thread.id.as_str()),
        Some("conversation-thread")
    );
    assert_eq!(
        delay_children(&Stanza::Message(message)).len(),
        1,
        "the typed thread and replay delay coexist"
    );
}

#[test]
fn resend_set_carries_each_stanzas_original_receipt_time() {
    // The queue records `original_receipt_at` per stanza; the resume
    // replay set must surface it alongside the XML so the caller can
    // stamp the XEP-0203 delay with the true send time — not the
    // resume time.
    let mut state = StreamManagementState::new();
    state.enable("stream-1178".to_string(), true, Some(300));

    let first_receipt = Utc.with_ymd_and_hms(2026, 7, 1, 9, 0, 0).unwrap();
    let second_receipt = Utc.with_ymd_and_hms(2026, 7, 1, 9, 5, 0).unwrap();
    let first = chat_message("a", "first", Vec::new());
    let second = chat_message("b", "second", Vec::new());
    let _ = state.record_outbound_with_receipt_at(
        first.clone(),
        first_receipt,
        SmUnackedStanzaPurpose::Application,
    );
    let _ = state.record_outbound_with_receipt_at(
        second.clone(),
        second_receipt,
        SmUnackedStanzaPurpose::Application,
    );

    let replay = state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].original_receipt_at, first_receipt);
    assert_eq!(stanza_to_xml(&replay[0].stanza), stanza_to_xml(&first));
    assert_eq!(replay[1].original_receipt_at, second_receipt);
    assert_eq!(stanza_to_xml(&replay[1].stanza), stanza_to_xml(&second));
}

#[tokio::test]
async fn xep0198_failed_resume_groupchat_retry_storage_deduplicates_mam_after_rejoin() {
    use jid::{BareJid, Jid};
    use waddle_xmpp::mam::{InMemoryMamStorage, MamStorage, StoreOutcome};
    use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedMucSender, ArchivedRichMessage};
    use waddle_xmpp_core::types::{Affiliation, Role};
    use waddle_xmpp_core::xep0359::OriginId;

    fn archived(id: &str, real_jid: &str, generation: u64) -> ArchivedMessage {
        ArchivedMessage {
            id: id.to_string(),
            body: Some("retry after failed resume".to_string()),
            origin_id: Some(OriginId::new("stable-sm-origin")),
            message_type: MessageType::Groupchat,
            nickname_generation: Some(generation),
            rich: Some(ArchivedRichMessage {
                muc_sender: Some(ArchivedMucSender {
                    jid: real_jid.parse::<Jid>().expect("real sender JID"),
                    affiliation: Affiliation::Member,
                    role: Role::Participant,
                }),
                ..ArchivedRichMessage::default()
            }),
            ..ArchivedMessage::for_test(
                "room@conference.example.com/alice"
                    .parse::<Jid>()
                    .expect("occupant JID"),
                "room@conference.example.com"
                    .parse::<Jid>()
                    .expect("room JID"),
            )
        }
    }

    // This crate cannot invoke the server interpreter, so this suite pins
    // the storage boundary required by XEP-0198 §Acks: a client may silently
    // resend after reconnect even though the peer already received the
    // stanza. XEP-0359's stable origin-id supplies the retry key.
    let storage = InMemoryMamStorage::new();
    let room = "room@conference.example.com"
        .parse::<BareJid>()
        .expect("room bare JID");
    let original = archived(
        "archive-before-disconnect",
        "alice@example.com/session-a",
        7,
    );
    let retry = archived("archive-after-rejoin", "alice@example.com/session-b", 8);

    assert_eq!(
        storage
            .store_message(&room, &original)
            .await
            .expect("store original"),
        StoreOutcome::Stored("archive-before-disconnect".to_string())
    );
    assert_eq!(
        storage
            .store_message(&room, &retry)
            .await
            .expect("store retry"),
        StoreOutcome::Deduplicated("archive-before-disconnect".to_string()),
        "XEP-0198 failed-resume resend keeps the original MAM identity"
    );
    assert_eq!(storage.count_messages(&room).await.expect("count"), 1);
}
