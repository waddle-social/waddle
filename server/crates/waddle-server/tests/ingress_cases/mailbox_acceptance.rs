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

/// A clustered PostgreSQL fixture at the mailbox epoch with the mailbox
/// selected, and the fence of the room claim it seeded.
async fn mailbox_fixture(name: &str) -> Option<(IngressFixture, RoomClaimFenceContext)> {
    let mut fixture = IngressFixture::postgres(name).await?;
    let room_fence = fixture.room_fence(&room()).await;
    fixture.uow = fixture
        .uow
        .clone()
        .with_remote_room_acceptance(RemoteRoomAcceptance::DurableMailbox);
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
/// no lane, whatever the selector. After the activation the relay selector
/// still writes none, and the message committed before the activation gets
/// no entry when it is retransmitted: it carries no source binding, so no
/// owner decision is opened for it.
#[tokio::test]
async fn commits_below_the_mailbox_epoch_or_under_the_relay_selector_write_no_entry() {
    let Some(mut fixture) = IngressFixture::postgres("mailbox_off").await else {
        return;
    };
    fixture.room_fence(&room()).await;
    assert!(
        fixture.uow.mailbox_acceptance_enabled(),
        "the mailbox is the default selector on clustered PostgreSQL"
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
    let relay_selected = fixture
        .uow
        .clone()
        .with_remote_room_acceptance(RemoteRoomAcceptance::OrderedRelay);
    assert!(!relay_selected.mailbox_acceptance_enabled());
    let relayed = origin_groupchat(&fixture, "mailbox-relay", "relay selector body");
    let relayed_key = commit_submission(&relay_selected, &relayed, 5)
        .await
        .expect("origin commit under the relay selector")
        .message_key
        .expect("canonical message");
    assert_eq!(entry_state(&fixture, relayed_key).await, (None, None));
    assert_eq!(fixture.count("ingress_mailbox_lanes").await, 0);

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
        waddle_server::config::IngressConfig {
            remote_room_acceptance: RemoteRoomAcceptance::DurableMailbox,
            ..Default::default()
        },
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
