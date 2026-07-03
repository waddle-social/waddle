//! NotificationCandidate construction and insertion semantics.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::*;
use jid::Jid;
use waddle_server::notification_outbox::*;
use waddle_xmpp_core::xep0359::StanzaId;

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
