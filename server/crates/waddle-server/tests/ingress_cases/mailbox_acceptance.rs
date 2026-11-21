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
use waddle_server::ingress_uow::MailboxRepository;
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

/// Advance the fixture's protocol epoch to 2, from which a commit carries
/// remote room acceptance through the mailbox. Each statement is one step
/// of the epoch guard and its own transaction.
async fn activate_mailbox_epoch(fixture: &IngressFixture) {
    for _ in 0..2 {
        fixture
            .execute(
                "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = now(), \
                 lineage_uuid = ?::uuid WHERE id = 1",
                waddle_server::db_params![uuid::Uuid::new_v4().to_string()],
            )
            .await;
    }
}

/// A clustered PostgreSQL fixture at the mailbox epoch, and the fence of
/// the room claim it seeded.
async fn mailbox_fixture(name: &str) -> Option<(IngressFixture, RoomClaimFenceContext)> {
    let mut fixture = IngressFixture::postgres(name).await?;
    let room_fence = fixture.room_fence(&room()).await;
    assert!(fixture.uow.mailbox_acceptance_enabled());
    activate_mailbox_epoch(&fixture).await;
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

/// A room refuses a stanza of a sender it has not seated by planning only
/// the XEP-0045 §7.4 error reply, with no rejection class. The owner's
/// commit records that plan as its refusal: the entry and the message are
/// decided as rejected and the error is stored as the rejection child the
/// origin answers its sender from. A replay keeps the decision.
#[tokio::test]
async fn owner_plan_with_only_an_error_reply_is_the_owners_refusal() {
    let Some((fixture, room_fence)) = mailbox_fixture("mailbox_room_refusal").await else {
        return;
    };
    let origin = origin_groupchat(&fixture, "mailbox-room-refusal", "not an occupant");
    let key = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit")
        .message_key
        .expect("canonical message");

    let mut owner = owner_groupchat(&origin, key, &room_fence);
    let error = waddle_xmpp::ingress::FrozenStanzaError::new(
        waddle_xmpp::ingress::FrozenStanzaErrorType::Cancel,
        waddle_xmpp::StanzaErrorCondition::NotAcceptable,
    );
    let mut reply = owner.plan.sanitized_message.clone();
    reply.to = Some(owner.sender.clone().into());
    reply.from = Some(room().into());
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.payloads.push(error.to_xmpp().into());
    owner.plan.error_reply = Some(waddle_xmpp::Stanza::Message(reply));
    owner.plan.intents = vec![IngressEffectIntent::ErrorReply {
        recipient: owner.sender.clone(),
        error,
    }];
    assert!(owner.plan.rejection.is_none());
    commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner refusal commits");
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::OwnerRejected), Some(2)),
        "the room's refusal is the owner's decision"
    );
    assert_eq!(fixture.count("ingress_owner_rejections").await, 1);

    commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner replay");
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::OwnerRejected), Some(2))
    );
    assert_eq!(fixture.count("ingress_owner_rejections").await, 1);
    fixture.close().await;
}

/// The cutover boundary is the protocol epoch each commit locks. Below the
/// mailbox epoch a clustered commit dispatches through the relay and writes
/// no lane. After the activation the message committed before it gets no
/// entry when it is retransmitted: it carries no source binding, so no
/// owner decision is opened for it.
#[tokio::test]
async fn commits_below_the_mailbox_epoch_write_no_entry() {
    let Some(mut fixture) = IngressFixture::postgres("mailbox_off").await else {
        return;
    };
    fixture.room_fence(&room()).await;
    assert!(
        fixture.uow.mailbox_acceptance_enabled(),
        "clustered PostgreSQL carries remote room acceptance through the mailbox"
    );
    let relay_era = origin_groupchat(&fixture, "mailbox-off", "relayed body");
    let relay_era_commit = commit_submission(&fixture.uow, &relay_era, 5)
        .await
        .expect("origin commit below the mailbox epoch");
    let relay_era_key = relay_era_commit.message_key.expect("canonical message");
    assert_eq!(relay_era_commit.remote_acceptance, None);
    assert_eq!(entry_state(&fixture, relay_era_key).await, (None, None));
    assert_eq!(fixture.count("ingress_mailbox_lanes").await, 0);

    activate_mailbox_epoch(&fixture).await;

    let replay = commit_submission(&fixture.uow, &relay_era, 5)
        .await
        .expect("retransmission after the activation");
    assert_eq!(replay.message_key, Some(relay_era_key));
    assert_eq!(
        replay.remote_acceptance, None,
        "a relay-era message awaits no owner decision"
    );
    assert_eq!(entry_state(&fixture, relay_era_key).await, (None, None));
    assert_eq!(fixture.count("ingress_mailbox_entries").await, 0);
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
        waddle_server::config::IngressConfig::default(),
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

/// An ingress authority on the fixture's database that runs as `identity`
/// in a cluster with the mailbox selected.
async fn mailbox_authority(
    fixture: &IngressFixture,
    identity: &waddle_xmpp::ownership::NodeIdentity,
) -> waddle_server::ingress::IngressAuthority {
    let deployment = fixture
        .optional_text("SELECT deployment_uuid FROM _lineage WHERE id = 1")
        .await
        .expect("enrolled deployment")
        .parse::<uuid::Uuid>()
        .expect("deployment UUID");
    let authority = waddle_server::ingress::IngressAuthority::try_new(
        waddle_server::config::IngressConfig::default(),
        fixture.db.clone(),
        waddle_server::config::LineageConfig {
            deployment_uuid: Some(waddle_server::db::lineage::DeploymentUuid(deployment)),
            action: None,
        },
        Some(waddle_xmpp::ownership::SharedNodeIdentity::new(
            identity.clone(),
        )),
    )
    .await
    .expect("mailbox ingress authority");
    assert!(authority.mailbox_acceptance_enabled());
    authority
}

/// The owner decided and the origin never read the decision. The sweep
/// consumes the entry once it is past the grace, so the canonical message is
/// no longer retained by it, and the decision stays readable: a late
/// retransmission of the stanza is counted without waiting.
#[tokio::test]
async fn sweep_consumes_a_decision_no_origin_read_and_keeps_it_readable() {
    let Some((fixture, room_fence)) = mailbox_fixture("mailbox_sweep").await else {
        return;
    };
    let authority = mailbox_authority(&fixture, &room_fence.owner).await;
    let origin = origin_groupchat(&fixture, "mailbox-sweep", "swept body");
    let committed = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit");
    let key = committed.message_key.expect("canonical message");

    // A pending entry is never swept, whatever its age.
    let sweep = authority
        .sweep_unconsumed_decisions(std::time::Duration::ZERO, None)
        .await
        .expect("sweep before the decision");
    assert_eq!((sweep.consumed, sweep.unproven), (0, 0));
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::Pending), Some(0))
    );

    commit_submission(&fixture.uow, &owner_groupchat(&origin, key, &room_fence), 5)
        .await
        .expect("owner acceptance");
    // Inside the grace the entry is left to its origin.
    let sweep = authority
        .sweep_unconsumed_decisions(std::time::Duration::from_secs(3600), None)
        .await
        .expect("sweep inside the grace");
    assert_eq!((sweep.consumed, sweep.unproven), (0, 0));
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::OwnerAccepted), Some(1))
    );

    let sweep = authority
        .sweep_unconsumed_decisions(std::time::Duration::ZERO, None)
        .await
        .expect("sweep past the grace");
    assert_eq!((sweep.consumed, sweep.unproven), (1, 0));
    assert_eq!(sweep.resume_after, None);
    assert_eq!(
        entry_state(&fixture, key).await,
        (
            Some(MailboxEntryDisposition::Consumed(
                MailboxOwnerDecision::Accepted
            )),
            Some(1)
        )
    );
    let again = authority
        .sweep_unconsumed_decisions(std::time::Duration::ZERO, None)
        .await
        .expect("second sweep");
    assert_eq!((again.consumed, again.unproven), (0, 0));

    // The origin's lookup and a retransmission both still read the decision.
    let pending = committed.remote_acceptance.expect("pending acceptance");
    assert_eq!(
        resolve_room_acceptance(&fixture.uow, &pending)
            .await
            .expect("lookup after the sweep"),
        RoomAcceptancePoll::Decided(MailboxOwnerDecision::Accepted)
    );
    let replay = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin replay");
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(replay.remote_acceptance, None);
    fixture.close().await;
}

/// The room's claim moves while an entry is pending. The successor's backlog
/// scan does not see the lane, which still names the predecessor. Its
/// adoption pass finds the lane, proves the room claim from the claims table
/// and writes it onto the lane; a node that does not hold the claim adopts
/// nothing. The successor then works the same entry from canonical rows.
#[tokio::test]
async fn successor_owner_adopts_the_lane_of_a_room_it_now_owns() {
    use waddle_server::ingress::mailbox_worker::MailboxLaneHead;
    use waddle_xmpp::ownership::NodeIdentity;
    let Some((fixture, room_fence)) = mailbox_fixture("mailbox_transfer").await else {
        return;
    };
    let predecessor = room_fence.owner.clone();
    let successor = NodeIdentity::new("successor-owner", "successor-incarnation");
    let bystander = NodeIdentity::new("bystander", "bystander-incarnation");
    let authority = mailbox_authority(&fixture, &successor).await;

    let origin = origin_groupchat(&fixture, "mailbox-transfer", "transferred body");
    let committed = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit");
    let key = committed.message_key.expect("canonical message");
    let lane = committed
        .remote_acceptance
        .as_ref()
        .expect("pending acceptance")
        .lane;

    // While the predecessor holds the claim nobody else adopts its lane.
    let adoption = authority
        .adopt_transferred_lanes(&successor, None, |_| async { true })
        .await
        .expect("adoption before the transfer");
    assert_eq!(adoption.adopted, 0);
    assert_eq!(
        authority
            .scan_mailbox_backlog(&predecessor, None)
            .await
            .expect("predecessor scan"),
        vec![lane]
    );

    // The room claim moves to the successor under a later epoch.
    fixture
        .execute(
            "UPDATE clustering_claims SET node_id = ?, node_epoch = ?, claim_epoch = ? \
             WHERE entity = ?",
            waddle_server::db_params![
                successor.node_id.clone(),
                successor.node_epoch.clone(),
                room_fence.epoch.0 + 6,
                format!("room_actor:{}", room())
            ],
        )
        .await;
    assert!(
        authority
            .scan_mailbox_backlog(&successor, None)
            .await
            .expect("successor scan before adoption")
            .is_empty(),
        "the lane still names the predecessor"
    );
    let adoption = authority
        .adopt_transferred_lanes(&bystander, None, |_| async { true })
        .await
        .expect("bystander adoption");
    assert_eq!(adoption.adopted, 0, "only the claim holder adopts");
    let adoption = authority
        .adopt_transferred_lanes(&successor, None, |_| async { true })
        .await
        .expect("successor adoption");
    assert_eq!(adoption.adopted, 1);
    assert_eq!(
        authority
            .scan_mailbox_backlog(&successor, None)
            .await
            .expect("successor scan"),
        vec![lane]
    );
    assert!(authority
        .scan_mailbox_backlog(&predecessor, None)
        .await
        .expect("predecessor scan after adoption")
        .is_empty());
    assert!(matches!(
        authority
            .claim_room_acceptance(lane, &predecessor)
            .await
            .expect("predecessor claim"),
        MailboxLaneHead::Idle
    ));

    // The successor rebuilds the same request the origin recorded.
    let MailboxLaneHead::Ready(claimed) = authority
        .claim_room_acceptance(lane, &successor)
        .await
        .expect("successor claim")
    else {
        panic!("the adopted lane's head is ready for the successor");
    };
    assert_eq!(claimed.operation, MailboxOperationKey::RoomAcceptance(key));
    assert_eq!(claimed.message, origin.plan.sanitized_message);
    assert_eq!(
        entry_state(&fixture, key).await,
        (Some(MailboxEntryDisposition::Pending), Some(0)),
        "adoption decides nothing"
    );
    fixture.close().await;
}

/// Session-scoped room requests (`RoomJoin`, `RoomPresence`, `RoomLeave`,
/// `RoomIq`) settle as `Rejected` when their retry budget is exhausted instead
/// of dead-lettering their lane. The lane cursor advances so subsequent queued
/// entries are immediately claimable, and `ingress_room_requests` is settled so
/// `count_unsettled_decisions` stays zero.
#[tokio::test]
async fn retry_exhausted_room_requests_settle_rejected_and_unblock_the_lane() {
    use std::time::Duration;
    use waddle_server::ingress::mailbox_worker::{
        MailboxAttemptRecord, MailboxLaneHead, MAILBOX_ATTEMPT_LIMIT,
    };
    use waddle_server::ingress::room_request::{RoomRequestCommit, RoomRequestSubmission};
    use waddle_server::ingress_uow::RoomRequestRepository;
    use waddle_xmpp::ingress::mailbox::{RoomRequestKey, RoomRequestKind};
    use waddle_xmpp::Stanza;
    use waddle_xmpp_core::OccupancySessionGeneration;

    let Some((fixture, room_fence)) = mailbox_fixture("room_req_exhaust").await else {
        return;
    };
    let owner = room_fence.owner.clone();
    let authority = mailbox_authority(&fixture, &owner).await;
    let occupant: jid::FullJid = "alice@example.com/web".parse().expect("occupant");
    let generation = OccupancySessionGeneration::mint();

    let presence_stanza = |kind: RoomRequestKind| -> Stanza {
        match kind {
            RoomRequestKind::Join | RoomRequestKind::Presence => {
                let mut p =
                    xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
                p.to = Some(room().with_resource_str("alice").expect("nick").into());
                Stanza::Presence(p)
            }
            RoomRequestKind::Leave => {
                let mut p = xmpp_parsers::presence::Presence::new(
                    xmpp_parsers::presence::Type::Unavailable,
                );
                p.to = Some(room().with_resource_str("alice").expect("nick").into());
                Stanza::Presence(p)
            }
            RoomRequestKind::Iq => Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::Get {
                from: Some(occupant.clone().into()),
                to: Some(room().into()),
                id: "room-iq-exhaust".to_owned(),
                payload: minidom::Element::builder("query", "jabber:iq:version").build(),
            })),
        }
    };

    // Queue two entries on the same MUC-stanza lane: RoomJoin followed by
    // RoomPresence. Exhausting retries on the head must auto-settle it and
    // unblock the queued second entry on the same lane.
    let RoomRequestCommit::Committed(join_pending) = authority
        .commit_room_request(RoomRequestSubmission {
            key: RoomRequestKey::mint(),
            kind: RoomRequestKind::Join,
            room: room(),
            occupant: occupant.clone(),
            generation,
            principal: fixture.principal.clone(),
            stanza: presence_stanza(RoomRequestKind::Join),
        })
        .await
        .expect("commit join")
    else {
        panic!("join request must be carried");
    };
    let RoomRequestCommit::Committed(presence_pending) = authority
        .commit_room_request(RoomRequestSubmission {
            key: RoomRequestKey::mint(),
            kind: RoomRequestKind::Presence,
            room: room(),
            occupant: occupant.clone(),
            generation,
            principal: fixture.principal.clone(),
            stanza: presence_stanza(RoomRequestKind::Presence),
        })
        .await
        .expect("commit presence")
    else {
        panic!("presence request must be carried");
    };
    assert_eq!(join_pending.lane, presence_pending.lane);

    for attempt in 1..MAILBOX_ATTEMPT_LIMIT {
        assert_eq!(
            authority
                .record_room_acceptance_failure(join_pending.lane, join_pending.operation, &owner)
                .await
                .expect("record deferred attempt"),
            MailboxAttemptRecord::Deferred { attempts: attempt }
        );
    }
    assert_eq!(
        authority
            .record_room_acceptance_failure(join_pending.lane, join_pending.operation, &owner)
            .await
            .expect("record exhausting attempt"),
        MailboxAttemptRecord::ExhaustedSettled
    );

    // Verify mailbox entry and parent request are settled cleanly with no dead
    // letter and no unsettled decision left behind.
    let mut tx = fixture.uow.begin().await.expect("inspect join state");
    let entry = MailboxRepository::lookup(&mut tx, join_pending.operation)
        .await
        .expect("lookup join entry")
        .expect("join entry exists");
    assert_eq!(
        entry.disposition,
        MailboxEntryDisposition::Consumed(MailboxOwnerDecision::Rejected)
    );
    assert_eq!(entry.dead_letter, None);
    assert_eq!(entry.attempts, MAILBOX_ATTEMPT_LIMIT);
    let stored = RoomRequestRepository::read(&mut tx, join_pending.key)
        .await
        .expect("read join request")
        .expect("join request exists");
    assert_eq!(stored.decision, Some(MailboxOwnerDecision::Rejected));
    assert!(stored.settled, "exhausted room request must be settled");
    assert_eq!(
        RoomRequestRepository::count_unsettled_decisions(&mut tx, Duration::ZERO)
            .await
            .expect("count unsettled"),
        0
    );
    drop(tx);

    // The queued second entry on the same lane is now immediately claimable!
    let MailboxLaneHead::ReadyRequest(next) = authority
        .claim_room_acceptance(join_pending.lane, &owner)
        .await
        .expect("claim next entry on unblocked lane")
    else {
        panic!("the second queued room request must be claimable after head exhaustion");
    };
    assert_eq!(next.operation, presence_pending.operation);

    // Exhaust Presence, Leave, and Iq as well to prove all four kinds settle.
    for (lane, operation, key) in {
        let mut items = vec![(
            presence_pending.lane,
            presence_pending.operation,
            presence_pending.key,
        )];
        for kind in [RoomRequestKind::Leave, RoomRequestKind::Iq] {
            let RoomRequestCommit::Committed(pending) = authority
                .commit_room_request(RoomRequestSubmission {
                    key: RoomRequestKey::mint(),
                    kind,
                    room: room(),
                    occupant: occupant.clone(),
                    generation,
                    principal: fixture.principal.clone(),
                    stanza: presence_stanza(kind),
                })
                .await
                .expect("commit request")
            else {
                panic!("{kind:?} request must be carried");
            };
            items.push((pending.lane, pending.operation, pending.key));
        }
        items
    } {
        for _ in 1..MAILBOX_ATTEMPT_LIMIT {
            authority
                .record_room_acceptance_failure(lane, operation, &owner)
                .await
                .expect("defer");
        }
        assert_eq!(
            authority
                .record_room_acceptance_failure(lane, operation, &owner)
                .await
                .expect("exhaust"),
            MailboxAttemptRecord::ExhaustedSettled
        );
        let mut tx = fixture.uow.begin().await.expect("inspect settled request");
        let entry = MailboxRepository::lookup(&mut tx, operation)
            .await
            .expect("lookup")
            .expect("entry");
        assert_eq!(
            entry.disposition,
            MailboxEntryDisposition::Consumed(MailboxOwnerDecision::Rejected)
        );
        assert_eq!(entry.dead_letter, None);
        let stored = RoomRequestRepository::read(&mut tx, key)
            .await
            .expect("read")
            .expect("stored");
        assert_eq!(stored.decision, Some(MailboxOwnerDecision::Rejected));
        assert!(stored.settled);
        drop(tx);
    }

    let mut tx = fixture.uow.begin().await.expect("check final state");
    assert!(
        MailboxRepository::list_dead_letters(&mut tx, 16)
            .await
            .expect("list dead letters")
            .is_empty(),
        "no session-scoped room request should be dead-lettered"
    );
    assert_eq!(
        RoomRequestRepository::count_unsettled_decisions(&mut tx, Duration::ZERO)
            .await
            .expect("count unsettled"),
        0
    );
    drop(tx);
    fixture.close().await;
}

/// A deposed room owner that hits the retry limit after the room claim has
/// moved to another node must NOT reject-settle the entry; it returns
/// `Settled` and leaves the entry pending so the new owner can adopt the lane
/// and process it cleanly.
#[tokio::test]
async fn deposed_room_owner_does_not_settle_exhausted_room_request() {
    use waddle_server::ingress::mailbox_worker::{
        MailboxAttemptRecord, MailboxLaneHead, MAILBOX_ATTEMPT_LIMIT,
    };
    use waddle_server::ingress::room_request::{RoomRequestCommit, RoomRequestSubmission};
    use waddle_server::ingress_uow::RoomRequestRepository;
    use waddle_xmpp::ingress::mailbox::{RoomRequestKey, RoomRequestKind};
    use waddle_xmpp::ownership::NodeIdentity;
    use waddle_xmpp::Stanza;
    use waddle_xmpp_core::OccupancySessionGeneration;

    let Some((fixture, room_fence)) = mailbox_fixture("room_req_deposed").await else {
        return;
    };
    let old_owner = room_fence.owner.clone();
    let new_owner = NodeIdentity::new("new-room-owner", "new-incarnation");
    let old_authority = mailbox_authority(&fixture, &old_owner).await;
    let new_authority = mailbox_authority(&fixture, &new_owner).await;
    let occupant: jid::FullJid = "bob@example.com/web".parse().expect("occupant");

    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.to = Some(room().with_resource_str("bob").expect("nick").into());
    let RoomRequestCommit::Committed(pending) = old_authority
        .commit_room_request(RoomRequestSubmission {
            key: RoomRequestKey::mint(),
            kind: RoomRequestKind::Presence,
            room: room(),
            occupant,
            generation: OccupancySessionGeneration::mint(),
            principal: fixture.principal.clone(),
            stanza: Stanza::Presence(presence),
        })
        .await
        .expect("commit presence")
    else {
        panic!("request must be carried");
    };

    // Bring attempts to MAILBOX_ATTEMPT_LIMIT - 1 with zero retry delay so
    // the successor can immediately claim the entry once it adopts the lane.
    {
        let mut tx = fixture.uow.begin().await.expect("bump attempts");
        let locked = MailboxRepository::claim_backlogged_lane(&mut tx, pending.lane, &old_owner)
            .await
            .expect("lock lane")
            .expect("lane belongs to old owner");
        for attempt in 1..MAILBOX_ATTEMPT_LIMIT {
            assert_eq!(
                MailboxRepository::record_attempt(
                    &mut tx,
                    &locked,
                    pending.operation,
                    std::time::Duration::ZERO,
                )
                .await
                .expect("record attempt"),
                attempt
            );
        }
        tx.commit().await.expect("commit attempts");
    }

    // Move the room claim to the successor before the old owner's final attempt.
    fixture
        .execute(
            "UPDATE clustering_claims SET node_id = ?, node_epoch = ?, claim_epoch = ? \
             WHERE entity = ?",
            waddle_server::db_params![
                new_owner.node_id.clone(),
                new_owner.node_epoch.clone(),
                room_fence.epoch.0 + 10,
                format!("room_actor:{}", room())
            ],
        )
        .await;

    assert_eq!(
        old_authority
            .record_room_acceptance_failure(pending.lane, pending.operation, &old_owner)
            .await
            .expect("deposed owner attempt"),
        MailboxAttemptRecord::ClaimNotHeld,
        "deposed owner must report ClaimNotHeld without settling the request"
    );
    assert!(
        matches!(
            old_authority
                .claim_room_acceptance(pending.lane, &old_owner)
                .await
                .expect("deposed owner re-claim"),
            MailboxLaneHead::Idle
        ),
        "deposed owner's retry must be deferred so work_lane does not hot-spin"
    );

    let mut tx = fixture.uow.begin().await.expect("inspect pending");
    let entry = MailboxRepository::lookup(&mut tx, pending.operation)
        .await
        .expect("lookup")
        .expect("entry");
    assert_eq!(entry.disposition, MailboxEntryDisposition::Pending);
    assert_eq!(entry.dead_letter, None);
    assert_eq!(entry.attempts, MAILBOX_ATTEMPT_LIMIT - 1);
    let stored = RoomRequestRepository::read(&mut tx, pending.key)
        .await
        .expect("read")
        .expect("stored");
    assert_eq!(stored.decision, None);
    assert!(!stored.settled);
    drop(tx);

    // The successor adopts the lane (which clears the deposed owner's backoff)
    // and can immediately work the pending request.
    let adoption = new_authority
        .adopt_transferred_lanes(&new_owner, None, |_| async { true })
        .await
        .expect("successor adopts lane");
    assert_eq!(adoption.adopted, 1);
    assert_eq!(
        new_authority
            .scan_mailbox_backlog(&new_owner, None)
            .await
            .expect("successor backlog scan"),
        vec![pending.lane]
    );
    assert!(matches!(
        new_authority
            .claim_room_acceptance(pending.lane, &new_owner)
            .await
            .expect("successor claims request"),
        MailboxLaneHead::ReadyRequest(claimed) if claimed.operation == pending.operation
    ));
    fixture.close().await;
}

/// Online operator dead-letter inspection and repair (`waddle-server
/// mailbox-dead-letters`) lists pending dead letters targeting any node
/// incarnation (including stopped ones), repairs `RetriesExhausted` entries
/// without matching `target_owner`, and refuses `CanonicalEvidenceInvalid`
/// unless `force_invalid_evidence` is explicitly passed.
#[tokio::test]
async fn operator_dead_letter_list_and_repair_works_for_stopped_owner_and_guards_invalid_evidence()
{
    use waddle_server::ingress::mailbox_worker::{
        list_mailbox_dead_letters, operator_repair_mailbox_dead_letter, MailboxAttemptRecord,
        OperatorMailboxRepairOutcome, MAILBOX_ATTEMPT_LIMIT,
    };
    use waddle_server::ingress_uow::MailboxDeadLetterReason;

    let Some((fixture, room_fence)) = mailbox_fixture("op_dead_letter_cli").await else {
        return;
    };
    let owner = room_fence.owner.clone();
    let authority = mailbox_authority(&fixture, &owner).await;

    let origin = origin_groupchat(&fixture, "op-dead-letter-1", "dead-lettered message");
    let committed = commit_submission(&fixture.uow, &origin, 5)
        .await
        .expect("origin commit");
    let key = committed.message_key.expect("canonical message");
    let lane = committed
        .remote_acceptance
        .as_ref()
        .expect("pending acceptance")
        .lane;
    let operation = MailboxOperationKey::RoomAcceptance(key);

    for _ in 0..MAILBOX_ATTEMPT_LIMIT {
        authority
            .record_room_acceptance_failure(lane, operation, &owner)
            .await
            .expect("record failure");
    }

    let listed = list_mailbox_dead_letters(&fixture.uow, 10)
        .await
        .expect("list dead letters");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].lane, lane);
    assert_eq!(listed[0].operation, operation);
    assert_eq!(listed[0].target_owner, owner);
    assert_eq!(listed[0].reason, MailboxDeadLetterReason::RetriesExhausted);

    assert_eq!(
        operator_repair_mailbox_dead_letter(&fixture.uow, lane, operation, false)
            .await
            .expect("operator repair"),
        OperatorMailboxRepairOutcome::Repaired {
            previous_reason: MailboxDeadLetterReason::RetriesExhausted,
        }
    );
    assert!(list_mailbox_dead_letters(&fixture.uow, 10)
        .await
        .expect("list after repair")
        .is_empty());
    assert_eq!(
        operator_repair_mailbox_dead_letter(&fixture.uow, lane, operation, false)
            .await
            .expect("repeat repair"),
        OperatorMailboxRepairOutcome::NotDeadLettered
    );

    // Park the same entry as `CanonicalEvidenceInvalid` and verify the safety
    // guard refuses repair unless `force_invalid_evidence` is true.
    let mut tx = fixture.uow.begin().await.expect("park invalid evidence");
    let locked = MailboxRepository::lock_lane_for_operator(&mut tx, lane)
        .await
        .expect("lock lane")
        .expect("lane exists");
    MailboxRepository::dead_letter(
        &mut tx,
        &locked,
        operation,
        MailboxDeadLetterReason::CanonicalEvidenceInvalid,
    )
    .await
    .expect("dead-letter invalid evidence");
    tx.commit().await.expect("commit invalid evidence");

    assert_eq!(
        operator_repair_mailbox_dead_letter(&fixture.uow, lane, operation, false)
            .await
            .expect("refused without force"),
        OperatorMailboxRepairOutcome::RefusedCanonicalEvidenceInvalid
    );
    assert_eq!(
        list_mailbox_dead_letters(&fixture.uow, 10)
            .await
            .expect("still listed")
            .len(),
        1
    );
    assert_eq!(
        operator_repair_mailbox_dead_letter(&fixture.uow, lane, operation, true)
            .await
            .expect("repaired with force"),
        OperatorMailboxRepairOutcome::Repaired {
            previous_reason: MailboxDeadLetterReason::CanonicalEvidenceInvalid,
        }
    );
    assert!(list_mailbox_dead_letters(&fixture.uow, 10)
        .await
        .expect("cleared after forced repair")
        .is_empty());

    // Confirm RoomAcceptance still dead-letters on retry exhaustion (unlike
    // session-scoped room requests).
    let mut last = MailboxAttemptRecord::Settled;
    for _ in 0..MAILBOX_ATTEMPT_LIMIT {
        last = authority
            .record_room_acceptance_failure(lane, operation, &owner)
            .await
            .expect("re-exhaust");
    }
    assert_eq!(last, MailboxAttemptRecord::DeadLettered);
    fixture.close().await;
}
