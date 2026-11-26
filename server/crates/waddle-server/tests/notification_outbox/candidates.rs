//! NotificationCandidate construction and insertion semantics.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::{bare, candidate, store};
use jid::Jid;
use waddle_server::notification_outbox::{
    NotificationCandidate, NotificationMessageHints, NotificationOutboxError,
};
use waddle_xmpp_core::xep0359::StanzaId;

#[cfg(feature = "test-support")]
mod envelopes {
    use super::bare;
    use waddle_server::notification_outbox::{
        candidate_from_envelope, GroupchatCandidateIdentity, NotificationCandidate,
        NotificationOutboxError, NotificationThreadId,
    };
    use waddle_server::test_support::direct_notification_candidate_from_envelope_for_test;
    use waddle_xmpp::xep::xep0334::{build_hint_element, Hint};
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;
    use waddle_xmpp::xep::xep0424::{build_retract_element, NS_MESSAGE_RETRACT};
    use waddle_xmpp::xep::xep0428::{set_fallback_payloads, FallbackIndication, FallbackRange};
    use waddle_xmpp::xep::xep0447::NS_SFS;
    use waddle_xmpp::xep::xep0461::{set_reply_payload, ReplyReference, NS_REPLY};
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::message::{Lang, Message};

    fn candidates(
        message: &Message,
    ) -> [Result<NotificationCandidate, NotificationOutboxError>; 2] {
        let owner = bare("alice@example.com");
        let room = bare("room@conference.example.com");
        [
            direct_notification_candidate_from_envelope_for_test(
                message,
                &owner,
                &"bob@example.com/web".parse().expect("sender"),
                &StanzaId::new("dm-archive", owner.clone().into()),
            ),
            candidate_from_envelope(
                message,
                GroupchatCandidateIdentity {
                    owner: &owner,
                    room: &room,
                    sender: &room.with_resource_str("bob").expect("occupant").into(),
                    thread_id: NotificationThreadId::root(),
                    archive_stanza_id: &StanzaId::new("room-archive", room.clone().into()),
                    is_live_occupant: false,
                    sender_can_broadcast_channel_mention: false,
                },
                &OccupantIdSecret::try_new(vec![7; 32]).expect("secret"),
            ),
        ]
    }

    fn message_with_body(body: &str, fallbacks: &[FallbackIndication]) -> Message {
        let mut message = Message::new(None);
        message.bodies.insert(Lang::new(), body.to_owned());
        set_fallback_payloads(&mut message, fallbacks);
        message
    }

    fn assert_preview(message: &Message, expected: Option<&str>) {
        for result in candidates(message) {
            let candidate = result.expect("notification candidate");
            assert_eq!(
                candidate.last_message_body(),
                expected,
                "{:?}",
                candidate.class()
            );
        }
    }

    #[test]
    fn xep0461_reply_preview_strips_quote_by_unicode_code_points() {
        let mut reply = message_with_body(
            "> 🙂 & tea\n  Yes, please.  ",
            &[FallbackIndication::for_range(NS_REPLY, 0, 10)],
        );
        set_reply_payload(&mut reply, &ReplyReference::new("original"));
        assert_preview(&reply, Some("Yes, please."));
    }

    #[test]
    fn xep0428_whole_body_fallback_leaves_no_preview() {
        let fallback = message_with_body("> earlier", &[FallbackIndication::whole_body(NS_REPLY)]);
        assert_preview(&fallback, None);
    }

    #[test]
    fn xep0428_ranges_are_combined_and_clamped_to_the_body() {
        let fallback = message_with_body(
            "quote keep tail",
            &[
                FallbackIndication::for_ranges(
                    NS_REPLY,
                    [
                        FallbackRange { start: 0, end: 5 },
                        FallbackRange { start: 3, end: 6 },
                    ],
                ),
                FallbackIndication::for_range(NS_MESSAGE_RETRACT, 10, 200),
                FallbackIndication::for_range(NS_REPLY, 300, 400),
            ],
        );
        assert_preview(&fallback, Some("keep"));
    }

    #[test]
    fn xep0428_fallback_ranges_that_leave_only_whitespace_omit_the_preview() {
        let fallback = message_with_body(
            "> quote\n  ",
            &[FallbackIndication::for_range(NS_REPLY, 0, 8)],
        );
        assert_preview(&fallback, None);
    }

    #[test]
    fn xep0447_file_share_fallback_stays_in_the_preview() {
        let url = "https://example.com/photo.jpg";
        let fallback = message_with_body(url, &[FallbackIndication::whole_body(NS_SFS)]);
        assert_preview(&fallback, Some(url));
    }

    #[test]
    fn xep0424_retraction_creates_no_candidate_even_without_fallback_indication() {
        let mut message = message_with_body("This message was retracted.", &[]);
        message.payloads.push(build_retract_element("original"));
        for result in candidates(&message) {
            assert!(
                matches!(
                    result,
                    Err(NotificationOutboxError::RetractionNotNotifiable)
                ),
                "expected retraction rejection, got {result:?}"
            );
        }
    }

    #[test]
    fn xep0334_no_store_drops_even_the_visible_reply_text() {
        let mut reply = message_with_body(
            "> quote\nPrivate reply",
            &[FallbackIndication::for_range(NS_REPLY, 0, 8)],
        );
        reply.payloads.push(build_hint_element(Hint::NoStore));
        for result in candidates(&reply) {
            let candidate = result.expect("notification candidate");
            assert!(candidate.no_store());
            assert_eq!(candidate.last_message_body(), None);
        }
    }
}

#[test]
fn candidate_snapshots_body_when_unhinted() {
    let candidate = candidate("archive-body")
        .with_last_message_body(Some("Wherefore art thou, Romeo?".to_string()));
    assert_eq!(
        candidate.last_message_body(),
        Some("Wherefore art thou, Romeo?")
    );
}

#[test]
fn candidate_drops_body_when_storage_hint_present() {
    // XEP-0334 storage conformance: an off-the-record body is never
    // persisted onto the candidate row, even temporarily.
    let recipient = bare("alice@example.com");
    let sender_jid: Jid = "bob@example.com/res".parse().expect("jid");
    for hints in [
        NotificationMessageHints::none().with_xep0334(true, false),
        NotificationMessageHints::none().with_xep0334(false, true),
    ] {
        let candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid.clone(),
            StanzaId::new("archive-hinted", Jid::from(recipient.clone())),
            false,
            hints,
        )
        .expect("candidate")
        .with_last_message_body(Some("secret".to_string()));
        assert_eq!(
            candidate.last_message_body(),
            None,
            "storage hint must drop the snapshotted body"
        );
    }
}

#[test]
fn direct_message_candidate_requires_full_sender_jid() {
    let recipient = bare("alice@example.com");
    let result = NotificationCandidate::direct_message(
        recipient.clone(),
        Jid::from(bare("bob@example.com")),
        StanzaId::new("archive-bare-sender", Jid::from(recipient)),
        false,
    );

    assert!(matches!(
        result,
        Err(NotificationOutboxError::SenderJidMissingResource(_))
    ));
}

/// Regression for self-DM structural-validity rejection at the
/// `NotificationCandidate::direct_message` constructor (#506
/// compliance: no push candidate/outbox entry for self-directed
/// notifications). Self-DM is *input validation*, not recipient-
/// state suppression, so it lives at the typed constructor
/// boundary alongside `require_full_sender_jid` and
/// `ArchiveStanzaIdOwnerMismatch`. No candidate row is ever
/// persisted, satisfying both:
///   (a) #506 Q3: T0 has no recipient-state reads — sender vs
///       recipient JID comparison is message-intrinsic provenance.
///   (b) compliance: self-notifications produce no candidate or
///       outbox entry.
#[tokio::test]
async fn self_directed_dm_candidate_is_rejected_at_constructor() {
    let recipient = bare("alice@example.com");
    let result = NotificationCandidate::direct_message(
        recipient.clone(),
        "alice@example.com/desktop"
            .parse()
            .expect("full self sender"),
        StanzaId::new("self-dm-archive", Jid::from(recipient.clone())),
        false,
    );
    assert!(matches!(
        result,
        Err(NotificationOutboxError::SelfDirectedNotificationCandidate(jid))
            if jid == recipient
    ));
}

/// Regression that the offline-delivery path silently drops self-
/// directed notification attempts without persisting anything to
/// `notification_candidates` or `notification_outbox`. End-to-end
/// surface of the constructor rejection above: insert is attempted
/// once, fails fast as a typed error, and the candidate table
/// stays empty.
#[tokio::test]
async fn self_directed_dm_inserts_no_candidate_row() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let result = NotificationCandidate::direct_message(
        recipient.clone(),
        "alice@example.com/desktop"
            .parse()
            .expect("full self sender"),
        StanzaId::new("self-dm-archive", Jid::from(recipient.clone())),
        false,
    );
    assert!(matches!(
        result,
        Err(NotificationOutboxError::SelfDirectedNotificationCandidate(
            _
        ))
    ));
    // No candidate insert attempted because the constructor refused
    // to produce one. Verify the candidate table is empty.
    assert!(
        store
            .pending_candidates(16)
            .await
            .expect("pending candidates")
            .is_empty(),
        "self-DM must not persist a candidate row"
    );
    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "self-DM must not persist an outbox job"
    );
}

#[tokio::test]
async fn call_candidate_round_trips_intrinsic_data_without_affecting_dm_identity() {
    use waddle_server::notification_outbox::{
        NotificationCall, NotificationClass, NotificationReason,
    };
    use xmpp_parsers::jingle::SessionId;
    let store = store().await;
    let recipient = bare("alice@example.com");
    let call = NotificationCandidate::call_with_hints(
        recipient.clone(),
        "bob@example.com/phone".parse().expect("sender"),
        StanzaId::new("call-archive", Jid::from(recipient)),
        NotificationCall {
            session_id: SessionId("call-session".into()),
            video: true,
        },
        NotificationMessageHints::none(),
    )
    .expect("call candidate");
    assert_eq!(call.class(), NotificationClass::Call);
    assert_eq!(call.reason(), NotificationReason::OfflineCall);
    store.insert_candidate(&call).await.expect("persist call");
    let pending = store.pending_candidates(16).await.expect("pending");
    assert_eq!(pending[0].call(), call.call());
    assert_eq!(pending[0].class(), NotificationClass::Call);
    assert!(candidate("dm-archive").call().is_none());
}

#[tokio::test]
async fn missed_call_round_trips_sid_without_video_and_deduplicates_across_retracts() {
    use crate::calls::missed_call_candidate;
    use waddle_server::notification_outbox::{
        NotificationCandidateInsertOutcome, NotificationClass, NotificationReason,
    };
    use xmpp_parsers::jingle::SessionId;

    let store = store().await;
    let missed = missed_call_candidate("first-retract", "session");
    assert_eq!(
        store.insert_candidate(&missed).await.expect("insert"),
        NotificationCandidateInsertOutcome::Inserted
    );
    let duplicate = missed_call_candidate("second-retract", "session");
    assert_eq!(
        store
            .insert_candidate(&duplicate)
            .await
            .expect("duplicate session"),
        NotificationCandidateInsertOutcome::Duplicate
    );
    let rows = store.pending_candidates(16).await.expect("pending");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].class(), NotificationClass::MissedCall);
    assert_eq!(rows[0].reason(), NotificationReason::CallRetracted);
    assert_eq!(
        rows[0].call_session_id(),
        Some(&SessionId("session".into()))
    );
    assert!(rows[0].call_video().is_none());
    assert!(rows[0].call().is_none());
    let mut raw = store
        .query(
            "SELECT call_sid,call_video FROM notification_candidates",
            (),
        )
        .await
        .expect("raw row");
    let row = raw.next().await.expect("read").expect("row");
    assert_eq!(row.get::<String>(0).expect("sid"), "session");
    assert!(row.get::<Option<i64>>(1).expect("video").is_none());
}
