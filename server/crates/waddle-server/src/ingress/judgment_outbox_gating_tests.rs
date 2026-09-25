//! #1831 Phase 2 MEDIUM finding: `apply_durable`'s `message_judgment_outbox`
//! gating logic (`ingress/durable.rs`, the `tx.judgment_outbox_enabled()`
//! block) had zero end-to-end coverage. `ingress_uow::judgment_outbox_tests`
//! and `message_judgment_outbox::store`'s own tests only exercise the
//! repository/SQL layer directly — none of them would catch a regression in
//! the actual gating conditions (e.g. flipping the `is_own_archive_copy`
//! comparison, or admitting `Existing`/`Repaired` alongside `Inserted`).
//!
//! These tests drive a real `commit_submission` call — the same entry point
//! production ingress uses — with the judgment-outbox flag enabled on the
//! unit of work, and assert on the resulting `message_judgment_outbox` row
//! count. `IngressFixture` (`super::test_support`) is this codebase's own
//! sqlite-backed ingress test fixture; `IngressUnitOfWork::set_judgment_outbox_enabled`
//! is called directly on its `uow` (mirroring how
//! `IngressAuthority::with_judgment_outbox_enabled` toggles the same flag in
//! production — `server::http::open_ingress_authority`).
use super::commit::commit_submission;
use super::test_support::IngressFixture;
use super::{IngressPrincipal, IngressStreamIdentity, IngressSubmission, PlannedEffect};
use crate::server::routes::interpret::effects::{
    direct::DurableDirectEffect,
    room::{DurableRoomEffect, RoomFenceRequirement},
    DurableEffect, Effect, IngressPlan, RoomExecutionPath,
};
use jid::{BareJid, Jid};
use waddle_xmpp::ingress::{
    ConnectionGeneration, DigestContext, DigestInput, IngressEffectIntent, NormalizedTarget,
    TransportGeneration,
};
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::{Lang, Message, MessageType};

/// Schema-initialized, judgment-outbox-enabled fixture. Every test that
/// needs the flag on builds from this; the "flag off" test builds its own
/// fixture instead and skips the `set_judgment_outbox_enabled(true)` call.
async fn enabled_fixture() -> IngressFixture {
    let mut fixture = IngressFixture::sqlite().await;
    crate::message_judgment_outbox::initialize(&fixture.db)
        .await
        .expect("judgment outbox schema");
    fixture.uow.set_judgment_outbox_enabled(true);
    fixture
}

async fn row_count(fixture: &IngressFixture) -> i64 {
    fixture.count("message_judgment_outbox").await
}

/// Append one `DurableDirectEffect::ArchiveDirect` planned effect (plus its
/// matching `ArchiveAuthoritative` intent) to `submission`, archiving
/// `body` under `archive` as if `from` sent it. Mirrors the shape
/// `tests/xep0313_ingress_archive.rs`'s `add_archive` and
/// `tests/ingress_cases/muc_progress_support.rs`'s
/// `archived_reflection_replay` already use to drive `apply_durable`
/// end-to-end without the full interpreter pipeline.
fn push_direct_archive(
    submission: &mut IngressSubmission,
    archive: &BareJid,
    from: Jid,
    to: Jid,
    id: &str,
    body: Option<&str>,
) {
    let stanza_id = StanzaId::new(id, archive.clone().into());
    let mut message = ArchivedMessage::for_test(from, to);
    message.id = id.to_owned();
    message.message_type = MessageType::Chat;
    message.body = body.map(str::to_owned);
    message.stanza_id = Some(stanza_id.clone());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: archive.clone(),
            stanza_id: stanza_id.clone(),
            by: archive.clone(),
            archived_at: message.timestamp,
            ordinal: None,
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive: archive.clone(),
                message: Box::new(message),
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))));
}

/// Same as [`push_direct_archive`] but for the room-owned
/// `DurableRoomEffect::ArchiveGroupchat` shape — the effect kind whose
/// `is_own_archive_copy` branch is unconditionally `true` (a groupchat
/// message has exactly one room-owned archive, never a sender/recipient
/// pair), unlike the `Direct` branch above.
fn push_room_archive(
    submission: &mut IngressSubmission,
    room: &BareJid,
    from: Jid,
    id: &str,
    body: Option<&str>,
) {
    let stanza_id = StanzaId::new(id, room.clone().into());
    let mut message = ArchivedMessage::for_test(from, room.clone().into());
    message.id = id.to_owned();
    message.message_type = MessageType::Groupchat;
    message.body = body.map(str::to_owned);
    message.stanza_id = Some(stanza_id.clone());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: room.clone(),
            stanza_id: stanza_id.clone(),
            by: room.clone(),
            archived_at: message.timestamp,
            ordinal: None,
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ArchiveGroupchat {
                room: room.clone(),
                message: Box::new(message),
                fence: RoomFenceRequirement::Unfenced,
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))));
}

/// A minimal, from-scratch groupchat submission: `fixture.submission` (the
/// shared fixture helper) always builds a `Chat`-typed 1:1 message, so a
/// groupchat gating test needs its own constructor. Mirrors
/// `tests/ingress_support.rs`'s own `submission()` shape (same
/// `IngressStreamIdentity::Ephemeral` identity, same digest construction),
/// with `to`/`type_` set for groupchat instead.
fn groupchat_submission(
    fixture: &IngressFixture,
    room: &BareJid,
    origin: &str,
    body: &str,
) -> IngressSubmission {
    let sender: jid::FullJid = fixture
        .principal
        .bare_jid()
        .with_resource_str("phone")
        .expect("sender resource");
    let mut message = Message::new(Some(Jid::from(room.clone())));
    message.from = Some(sender.clone().into());
    message.type_ = MessageType::Groupchat;
    message.bodies.insert(Lang::new(), body.to_owned());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, origin);
    let target = NormalizedTarget::Bare(room.clone());
    let digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    IngressSubmission {
        sender: sender.clone(),
        identity: IngressStreamIdentity::Ephemeral {
            principal: fixture.principal.clone(),
        },
        principal: IngressPrincipal::Authenticated(fixture.principal.clone()),
        target,
        plan: IngressPlan {
            failure: None,
            plan: Vec::new(),
            intents: Vec::new(),
            room_canonical_message: None,
            sanitized_message: message,
            error_reply: None,
            rejection: None,
            room_execution: RoomExecutionPath::None,
        },
        digest_input,
        connection_generation: TransportGeneration::Connection(ConnectionGeneration::INITIAL),
    }
}

/// Retarget a base (romeo -> juliet) submission into a self-DM (romeo ->
/// romeo), recomputing the digest the same way
/// `tests/ingress_cases/muc_progress_support.rs`'s `refresh_digest` does
/// after mutating `target`/`sanitized_message`.
fn retarget_self_dm(fixture: &IngressFixture, submission: &mut IngressSubmission) {
    let romeo = fixture.principal.bare_jid().clone();
    submission.target = NormalizedTarget::Bare(romeo.clone());
    submission.plan.sanitized_message.to = Some(romeo.clone().into());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
}

#[tokio::test]
async fn direct_message_enqueues_exactly_one_row_not_two() {
    let fixture = enabled_fixture().await;
    let mut submission = fixture.submission(Some("judgment-direct"), "hello there");
    let sender = fixture.principal.bare_jid().clone();
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    // Real 1:1 delivery archives twice: once into the sender's own MAM
    // store, once into the recipient's -- both driven through the same
    // `DurableDirectEffect::ArchiveDirect` shape with the same `from`.
    push_direct_archive(
        &mut submission,
        &sender,
        sender.clone().into(),
        recipient.clone().into(),
        "sender-copy",
        Some("hello there"),
    );
    push_direct_archive(
        &mut submission,
        &recipient,
        sender.clone().into(),
        recipient.clone().into(),
        "recipient-copy",
        Some("hello there"),
    );

    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    assert!(decision.class.advances());
    assert_eq!(
        row_count(&fixture).await,
        1,
        "only the sender's own archive copy enqueues a judgment row, not the recipient's too"
    );
    fixture.close().await;
}

#[tokio::test]
async fn self_dm_enqueues_exactly_one_row() {
    let fixture = enabled_fixture().await;
    let mut submission = fixture.submission(Some("judgment-self-dm"), "note to self");
    retarget_self_dm(&fixture, &mut submission);
    let romeo = fixture.principal.bare_jid().clone();
    push_direct_archive(
        &mut submission,
        &romeo,
        romeo.clone().into(),
        romeo.clone().into(),
        "self-dm-copy",
        Some("note to self"),
    );

    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    assert!(decision.class.advances());
    assert_eq!(row_count(&fixture).await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn groupchat_message_enqueues_exactly_one_row() {
    let fixture = enabled_fixture().await;
    let room: BareJid = "waddlers@muc.example.com".parse().expect("room");
    let mut submission = groupchat_submission(&fixture, &room, "judgment-groupchat", "hello room");
    let sender: Jid = fixture
        .principal
        .bare_jid()
        .clone()
        .with_resource_str("phone")
        .expect("sender resource")
        .into();
    push_room_archive(
        &mut submission,
        &room,
        sender,
        "room-copy",
        Some("hello room"),
    );

    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    assert!(decision.class.advances());
    assert_eq!(row_count(&fixture).await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn retransmit_of_the_same_message_enqueues_no_additional_rows() {
    let fixture = enabled_fixture().await;
    let mut submission = fixture.submission(Some("judgment-retransmit"), "hello there");
    let sender = fixture.principal.bare_jid().clone();
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    push_direct_archive(
        &mut submission,
        &sender,
        sender.clone().into(),
        recipient.clone().into(),
        "sender-copy",
        Some("hello there"),
    );

    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("first commit");
    assert!(first.class.advances());
    assert_eq!(row_count(&fixture).await, 1);

    // Same origin id, same content: a real retransmission (e.g. a client
    // retry after a dropped ack). The second commit must resolve to the
    // already-recorded archive authority (`MamTxStoreOutcome::Existing`,
    // not `Inserted`) and enqueue nothing further.
    let second = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("retransmit commit");
    assert!(second.class.advances());
    assert_eq!(
        row_count(&fixture).await,
        1,
        "a retransmit of an already-archived message must not enqueue again"
    );
    fixture.close().await;
}

#[tokio::test]
async fn message_with_no_body_enqueues_zero_rows() {
    let fixture = enabled_fixture().await;
    let mut submission = fixture.submission(Some("judgment-no-body"), "");
    let sender = fixture.principal.bare_jid().clone();
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    push_direct_archive(
        &mut submission,
        &sender,
        sender.clone().into(),
        recipient.clone().into(),
        "no-body-copy",
        None,
    );

    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    assert!(decision.class.advances());
    assert_eq!(row_count(&fixture).await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn empty_and_whitespace_only_bodies_enqueue_zero_rows() {
    let fixture = enabled_fixture().await;
    let sender = fixture.principal.bare_jid().clone();
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    for (origin, id, body) in [
        ("judgment-empty-body", "empty-body-copy", ""),
        (
            "judgment-whitespace-body",
            "whitespace-body-copy",
            "   \t\n",
        ),
    ] {
        let mut submission = fixture.submission(Some(origin), "placeholder");
        push_direct_archive(
            &mut submission,
            &sender,
            sender.clone().into(),
            recipient.clone().into(),
            id,
            Some(body),
        );
        let decision = commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("commit");
        assert!(decision.class.advances());
    }
    assert_eq!(
        row_count(&fixture).await,
        0,
        "an empty or whitespace-only body must never enqueue a judgment row"
    );
    fixture.close().await;
}

#[tokio::test]
async fn flag_off_by_default_never_enqueues_a_row() {
    // Deliberately does not call `set_judgment_outbox_enabled` -- the
    // default, production-off state.
    let fixture = IngressFixture::sqlite().await;
    crate::message_judgment_outbox::initialize(&fixture.db)
        .await
        .expect("judgment outbox schema");
    let mut submission = fixture.submission(Some("judgment-flag-off"), "hello there");
    let sender = fixture.principal.bare_jid().clone();
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    push_direct_archive(
        &mut submission,
        &sender,
        sender.clone().into(),
        recipient.clone().into(),
        "flag-off-copy",
        Some("hello there"),
    );

    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    assert!(decision.class.advances());
    assert_eq!(row_count(&fixture).await, 0);
    fixture.close().await;
}
