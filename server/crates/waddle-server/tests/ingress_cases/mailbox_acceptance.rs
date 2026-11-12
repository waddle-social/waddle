//! Remote room acceptance through the durable mailbox: the origin's commit
//! appends the pending entry, and the owner's commit records its decision on
//! that entry and on the canonical message in one transaction.

use super::{commit_submission, IngressDecisionClass, IngressEffectIntent, IngressFixture};
use waddle_server::ingress::{
    IngressCanonicalRef, IngressStreamIdentity, IngressSubmission, RoomExecutionPath,
};
use waddle_server::ingress_uow::{MailboxRepository, RemoteRoomAcceptance};
use waddle_xmpp::ingress::mailbox::{MailboxEntryDisposition, MailboxOperationKey};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, MessageKey, NormalizedTarget, RelayTargetIdentity,
};
use waddle_xmpp::muc::RoomClaimFenceContext;

fn room() -> jid::BareJid {
    "room@conference.example.com".parse().expect("room")
}

/// A groupchat submission as the sender's node plans it for a room another
/// node owns.
fn origin_groupchat(fixture: &IngressFixture, origin: &str, body: &str) -> IngressSubmission {
    let room = room();
    let mut submission = fixture.submission(Some(origin), body);
    let message = &mut submission.plan.sanitized_message;
    message.to = Some(room.clone().into());
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.digest_input = DigestInput::from_parsed(
        message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![submission.sender.to_bare(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    let relay_target = RelayTargetIdentity::owner_node("ingress-owner", "ingress-incarnation");
    submission.plan.intents = vec![IngressEffectIntent::DispatchToRoomRemote {
        room: room.clone(),
        relay_target: relay_target.clone(),
    }];
    submission.plan.room_execution = RoomExecutionPath::Remote { room, relay_target };
    submission
}

/// The same message as the room owner receives it for its own commit.
fn owner_groupchat(
    origin: &IngressSubmission,
    key: MessageKey,
    room_fence: &RoomClaimFenceContext,
) -> IngressSubmission {
    let mut owner = origin.clone();
    owner.plan.intents.clear();
    owner.plan.room_execution = RoomExecutionPath::None;
    owner.identity = IngressStreamIdentity::Relayed {
        canonical: IngressCanonicalRef {
            message_key: key,
            sender_bare: origin.sender.to_bare(),
            origin_id: origin.digest_input.origin().cloned(),
        },
        room: room(),
        room_fence: room_fence.clone(),
    };
    owner
}

async fn entry_state(
    fixture: &IngressFixture,
    key: MessageKey,
) -> (Option<MailboxEntryDisposition>, Option<i64>) {
    let mut tx = fixture.uow.begin().await.expect("begin lookup");
    let disposition = MailboxRepository::lookup(&mut tx, MailboxOperationKey::RoomAcceptance(key))
        .await
        .expect("mailbox lookup")
        .map(|entry| entry.disposition);
    drop(tx);
    let conn = fixture.db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT owner_decision::bigint FROM ingress_messages WHERE message_key = ?::uuid",
            waddle_server::db_params![key.to_storage().to_string()],
        )
        .await
        .expect("owner decision");
    let decision = rows
        .next()
        .await
        .expect("owner decision row")
        .expect("canonical message")
        .get::<Option<i64>>(0)
        .expect("owner decision column");
    (disposition, decision)
}

async fn mailbox_fixture(name: &str) -> Option<(IngressFixture, RoomClaimFenceContext)> {
    let mut fixture = IngressFixture::postgres(name).await?;
    let room_fence = fixture.room_fence(&room()).await;
    fixture.uow = fixture
        .uow
        .clone()
        .with_remote_room_acceptance(RemoteRoomAcceptance::DurableMailbox);
    assert!(fixture.uow.mailbox_acceptance_enabled());
    Some((fixture, room_fence))
}

#[tokio::test]
async fn origin_appends_pending_entry_and_owner_accepts_in_one_transaction() {
    let Some((fixture, room_fence)) = mailbox_fixture("mailbox_accept").await else {
        return;
    };
    let origin = origin_groupchat(&fixture, "mailbox-accept", "accepted body");
    let accepted = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit");
    let key = accepted.message_key.expect("canonical message");
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::Pending), Some(0)),
        "the origin opens the decision and appends the pending entry"
    );
    assert_eq!(fixture.count("ingress_mailbox_entries").await, 1);

    // A retransmission of the same stanza finds the one entry.
    let retry = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin retry");
    assert_eq!(retry.message_key, Some(key));
    assert_eq!(fixture.count("ingress_mailbox_entries").await, 1);

    let owner = owner_groupchat(&origin, key, &room_fence);
    let first = commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner acceptance");
    assert_eq!(first.class, IngressDecisionClass::OwnerFirstAcceptance);
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::OwnerAccepted), Some(1)),
        "the owner's commit carries its decision on the entry and the message"
    );
    assert_eq!(fixture.count("ingress_owner_rejections").await, 0);

    // The owner's replay changes neither the latch nor the entry.
    commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner replay");
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::OwnerAccepted), Some(1))
    );
    fixture.close().await;
}

#[tokio::test]
async fn owner_rejection_records_its_child_and_decides_the_entry() {
    use waddle_server::ingress::effects::{AuthorizationDeniedReason, PlanRejection};
    let Some((fixture, room_fence)) = mailbox_fixture("mailbox_reject").await else {
        return;
    };
    let origin = origin_groupchat(&fixture, "mailbox-reject", "refused body");
    let key = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit")
        .message_key
        .expect("canonical message");

    let mut owner = owner_groupchat(&origin, key, &room_fence);
    let error = waddle_xmpp::ingress::FrozenStanzaError::new(
        waddle_xmpp::ingress::FrozenStanzaErrorType::Cancel,
        waddle_xmpp::StanzaErrorCondition::Forbidden,
    );
    let mut reply = owner.plan.sanitized_message.clone();
    reply.to = Some(owner.sender.clone().into());
    reply.from = Some(room().into());
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.payloads.push(error.to_xmpp().into());
    owner.plan.rejection = Some(PlanRejection::AuthorizationDenied(
        AuthorizationDeniedReason::Forbidden,
    ));
    owner.plan.error_reply = Some(waddle_xmpp::Stanza::Message(reply));
    owner.plan.intents = vec![IngressEffectIntent::ErrorReply {
        recipient: owner.sender.clone(),
        error,
    }];
    let refused = commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner rejection commits");
    assert_eq!(refused.class, IngressDecisionClass::AuthorizationDenied);
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::OwnerRejected), Some(2)),
        "a refusal is the owner's decision too"
    );
    assert_eq!(fixture.count("ingress_owner_rejections").await, 1);
    assert_eq!(
        fixture.count("ingress_messages").await,
        2,
        "the frozen error is stored as the rejection child"
    );
    fixture.close().await;
}

#[tokio::test]
async fn relay_selector_writes_no_mailbox_entry() {
    let Some(mut fixture) = IngressFixture::postgres("mailbox_off").await else {
        return;
    };
    fixture.room_fence(&room()).await;
    assert!(!fixture.uow.mailbox_acceptance_enabled());
    let origin = origin_groupchat(&fixture, "mailbox-off", "relayed body");
    let key = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit")
        .message_key
        .expect("canonical message");
    assert_eq!(entry_state(&fixture, key).await, (None, None));
    assert_eq!(fixture.count("ingress_mailbox_lanes").await, 0);
    fixture.close().await;
}
