//! Remote room acceptance through the durable mailbox: the origin's commit
//! appends the pending entry, and the owner's commit records its decision on
//! that entry and on the canonical message in one transaction.

use super::{commit_submission, IngressDecisionClass, IngressEffectIntent, IngressFixture};
use waddle_server::ingress::origin_gate::{
    await_room_acceptance, resolve_room_acceptance, RoomAcceptancePoll, RoomAcceptanceUnresolved,
    RoomAcceptanceWatch,
};
use waddle_server::ingress::{
    IngressCanonicalRef, IngressStreamIdentity, IngressSubmission, RoomExecutionPath,
};
use waddle_server::ingress_uow::{MailboxRepository, RemoteRoomAcceptance};
use waddle_xmpp::ingress::mailbox::{
    MailboxEntryDisposition, MailboxOperationKey, MailboxOwnerDecision,
};
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

#[tokio::test]
async fn origin_reads_the_owner_decision_before_counting_the_stanza() {
    let Some((fixture, room_fence)) = mailbox_fixture("mailbox_gate").await else {
        return;
    };
    let origin = origin_groupchat(&fixture, "mailbox-gate", "gated body");
    let committed = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit");
    let key = committed.message_key.expect("canonical message");
    let pending = committed
        .remote_acceptance
        .clone()
        .expect("the commit reports the stanza as awaiting its owner");
    assert_eq!(pending.operation, MailboxOperationKey::RoomAcceptance(key));

    // Nothing but the owner's recorded decision resolves the wait.
    assert_eq!(
        resolve_room_acceptance(&fixture.uow, &pending)
            .await
            .expect("lookup"),
        RoomAcceptancePoll::Pending
    );
    let impatient = RoomAcceptanceWatch {
        budget: std::time::Duration::from_millis(60),
        first_interval: std::time::Duration::from_millis(10),
        max_interval: std::time::Duration::from_millis(20),
    };
    assert_eq!(
        await_room_acceptance(&fixture.uow, &pending, impatient, || false).await,
        Err(RoomAcceptanceUnresolved::BudgetExhausted),
        "an elapsed budget is not an acceptance"
    );
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::Pending), Some(0))
    );

    let owner = owner_groupchat(&origin, key, &room_fence);
    commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner acceptance");
    assert_eq!(
        await_room_acceptance(&fixture.uow, &pending, impatient, || false).await,
        Ok(MailboxOwnerDecision::Accepted)
    );
    assert_eq!(
        entry_state(&fixture, key).await,
        (
            Some(MailboxEntryDisposition::Consumed(
                MailboxOwnerDecision::Accepted
            )),
            Some(1)
        ),
        "reading the decision consumes the entry and keeps which decision it was"
    );
    // A second lookup, as after a lost reply, answers the same decision.
    assert_eq!(
        resolve_room_acceptance(&fixture.uow, &pending)
            .await
            .expect("repeat lookup"),
        RoomAcceptancePoll::Decided(MailboxOwnerDecision::Accepted)
    );
    // A retransmission after the decision no longer waits.
    let replay = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin replay");
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(replay.remote_acceptance, None);
    fixture.close().await;
}

#[tokio::test]
async fn retransmission_after_the_owner_decided_consumes_in_its_own_commit() {
    let Some((fixture, room_fence)) = mailbox_fixture("mailbox_replay").await else {
        return;
    };
    let origin = origin_groupchat(&fixture, "mailbox-replay", "replayed body");
    let key = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit")
        .message_key
        .expect("canonical message");
    commit_submission(&fixture.uow, &owner_groupchat(&origin, key, &room_fence), 5)
        .await
        .expect("owner acceptance");
    // The origin never read the decision (its watcher was lost). The same
    // stanza arrives again and finds the decision already recorded.
    let replay = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin replay");
    assert_eq!(replay.remote_acceptance, None);
    assert_eq!(
        entry_state(&fixture, key).await,
        (
            Some(MailboxEntryDisposition::Consumed(
                MailboxOwnerDecision::Accepted
            )),
            Some(1)
        )
    );
    fixture.close().await;
}

#[tokio::test]
async fn poller_works_the_lane_head_from_canonical_rows_and_parks_it_after_its_retries() {
    use waddle_server::ingress::mailbox_worker::{
        MailboxAttemptRecord, MailboxLaneHead, MAILBOX_ATTEMPT_LIMIT,
    };
    use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};
    let Some((fixture, room_fence)) = mailbox_fixture("mailbox_poller").await else {
        return;
    };
    let owner_identity = room_fence.owner.clone();
    // An ingress authority on the fixture's database that runs as the room's
    // owner in a cluster with the mailbox selected.
    let deployment = fixture
        .optional_text("SELECT deployment_uuid FROM _lineage WHERE id = 1")
        .await
        .expect("enrolled deployment")
        .parse::<uuid::Uuid>()
        .expect("deployment UUID");
    let authority = waddle_server::ingress::IngressAuthority::try_new(
        waddle_server::config::IngressConfig {
            remote_room_acceptance: RemoteRoomAcceptance::DurableMailbox,
            ..Default::default()
        },
        fixture.db.clone(),
        waddle_server::config::LineageConfig {
            deployment_uuid: Some(waddle_server::db::lineage::DeploymentUuid(deployment)),
            action: None,
        },
        Some(SharedNodeIdentity::new(owner_identity.clone())),
    )
    .await
    .expect("mailbox ingress authority");
    assert!(authority.mailbox_acceptance_enabled());

    let origin = origin_groupchat(&fixture, "mailbox-poller", "polled body");
    let committed = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit");
    let key = committed.message_key.expect("canonical message");
    let lane = committed
        .remote_acceptance
        .as_ref()
        .expect("pending acceptance")
        .lane;

    // The scan names the lane only to the node it targets. No hint was sent.
    assert_eq!(
        authority
            .scan_mailbox_backlog(&owner_identity, None)
            .await
            .expect("scan"),
        vec![lane]
    );
    let stranger = NodeIdentity::new("another-node", "another-incarnation");
    assert!(authority
        .scan_mailbox_backlog(&stranger, None)
        .await
        .expect("scan")
        .is_empty());
    assert!(matches!(
        authority
            .claim_room_acceptance(lane, &stranger)
            .await
            .expect("claim"),
        MailboxLaneHead::Idle
    ));

    // The head entry is rebuilt from canonical rows alone.
    let MailboxLaneHead::Ready(claimed) = authority
        .claim_room_acceptance(lane, &owner_identity)
        .await
        .expect("claim")
    else {
        panic!("the pending head entry is ready for the owner");
    };
    assert_eq!(claimed.operation, MailboxOperationKey::RoomAcceptance(key));
    assert_eq!(claimed.room, room());
    assert_eq!(claimed.message, origin.plan.sanitized_message);
    assert_eq!(claimed.admission.canonical.message_key, key);
    assert_eq!(
        claimed.admission.canonical.origin_id.as_ref(),
        origin.digest_input.origin()
    );
    assert_eq!(&claimed.admission.principal, &fixture.principal);
    assert_eq!(claimed.admission.stanza_lang, None);

    // A failed attempt defers the next one; the entry keeps its place.
    assert_eq!(
        authority
            .record_room_acceptance_failure(lane, claimed.operation, &owner_identity)
            .await
            .expect("record failure"),
        MailboxAttemptRecord::Deferred { attempts: 1 }
    );
    assert!(matches!(
        authority
            .claim_room_acceptance(lane, &owner_identity)
            .await
            .expect("claim"),
        MailboxLaneHead::Idle
    ));
    // The retry budget ends in a dead letter, never in a skipped entry.
    let mut last = MailboxAttemptRecord::Deferred { attempts: 1 };
    for _ in 1..MAILBOX_ATTEMPT_LIMIT {
        last = authority
            .record_room_acceptance_failure(lane, claimed.operation, &owner_identity)
            .await
            .expect("record failure");
    }
    assert_eq!(last, MailboxAttemptRecord::DeadLettered);
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::Pending), Some(0)),
        "a dead letter stays pending at the lane head"
    );
    assert_eq!(
        authority
            .scan_mailbox_backlog(&owner_identity, None)
            .await
            .expect("scan"),
        vec![lane],
        "the cursor did not pass the dead letter"
    );
    assert!(matches!(
        authority
            .claim_room_acceptance(lane, &owner_identity)
            .await
            .expect("claim"),
        MailboxLaneHead::Idle
    ));

    // Only an operator repair returns it to work.
    assert!(authority
        .repair_mailbox_dead_letter(lane, claimed.operation, &owner_identity)
        .await
        .expect("repair"));
    let MailboxLaneHead::Ready(repaired) = authority
        .claim_room_acceptance(lane, &owner_identity)
        .await
        .expect("claim")
    else {
        panic!("a repaired entry is ready again");
    };

    // The owner's commit, driven from the claimed entry, decides it and
    // drains the lane.
    let mut owner = owner_groupchat(&origin, key, &room_fence);
    owner.plan.sanitized_message = repaired.message.clone();
    commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner acceptance");
    assert!(authority
        .scan_mailbox_backlog(&owner_identity, None)
        .await
        .expect("scan")
        .is_empty());
    fixture.close().await;
}
