//! Durable XEP-0198 resume, compensation and expiry over single-node SQLite.
//!
//! A resume binds the next generation to the resuming attempt in one
//! transaction and fences the previous owner whether or not its transport
//! closes. Only a known rejection yields a fresh-bind permit. Cross-node
//! claim fencing, simultaneous resume from two processes and
//! commit-uncertainty need PostgreSQL fixtures and are not covered.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use jid::FullJid;
use uuid::Uuid;
use waddle_server::db::Database;
use waddle_server::sm_authority::{
    AdmissionAuthority, AdmissionError, AdmissionFrame, AdmissionRejection, AdmissionRequest,
    DetachSnapshot, DurableSmStore, EnableOperation, EnableRequest, EnabledCommit, LifecycleError,
    LifecycleRejection, OwnerError, OwnerRejection, ResumeError, ResumeOperation,
    ResumeRejectionReason, ResumeRequest, ResumeResolution, ResumedCommit,
};
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::durable::{
    DurableSmState, Exposure, ExposureBounds, FenceError, HandledError, OwnerFence,
    PrincipalStanding, ResumeDenial, SmOperationId, SmOrdinal, TerminalReason,
};
use waddle_xmpp::stream_management::persistence::{PersistedSession, SmReplayPayload};
use waddle_xmpp::stream_management::SmFrameProvenance;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::message::{Lang, Message};
use xmpp_parsers::presence::Show;

fn principal(context: u128, epoch: u64, jid: &str) -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        jid.parse().expect("bare jid"),
        AuthContextId::new(Uuid::from_u128(context)),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::new(epoch),
    )
}

fn alice() -> AuthenticatedPrincipalRef {
    principal(1, 7, "alice@example.test")
}

fn binding() -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + seconds, 0)
        .single()
        .expect("valid time")
}

fn alice_phone() -> FullJid {
    "alice@example.test/phone".parse().expect("full jid")
}

fn ordinal(value: u64) -> SmOrdinal {
    SmOrdinal::new(value).expect("test ordinal")
}

async fn store(name: &str) -> DurableSmStore {
    let db = Database::in_memory(name).await.expect("in-memory database");
    let bounds = ExposureBounds::new(8, 8192).expect("test bounds");
    DurableSmStore::open(db, bounds, false)
        .await
        .expect("open durable SM store")
}

async fn enabled(store: &DurableSmStore, owner: &BindingRef) -> EnabledCommit {
    store
        .enable(
            &EnableOperation::mint(),
            EnableRequest {
                binding: owner,
                principal: &alice(),
                full_jid: &alice_phone(),
                max_resume: Duration::from_secs(300),
                claim: None,
                now: at(0),
            },
        )
        .await
        .expect("enable commits")
}

fn frame(body: &str) -> AdmissionFrame {
    let mut message = Message::new(Some(alice_phone().into()));
    message.bodies.insert(Lang::new(), body.to_owned());
    AdmissionFrame {
        operation: SmOperationId::mint(),
        payload: SmReplayPayload::from(Box::new(Stanza::Message(message))),
        original_receipt_at: at(1),
        ingress_receipts: Vec::new(),
        provenance: SmFrameProvenance::NonIngress,
    }
}

async fn admit_owned(store: &DurableSmStore, fence: &OwnerFence, body: &str) -> SmOrdinal {
    store
        .admit(
            AdmissionRequest {
                authority: AdmissionAuthority::Owner {
                    fence,
                    principal: &alice(),
                },
                claim: None,
                now: at(2),
            },
            &frame(body),
        )
        .await
        .expect("owner admission")
        .ordinal()
}

fn snapshot(inbound_handled: u64) -> DetachSnapshot {
    DetachSnapshot {
        resource: PersistedSession {
            // The authority replaces the identity, counters and detach time.
            stream_id: SmSessionId::new("replaced"),
            user_id: "alice".to_owned(),
            jid: alice_phone(),
            occupancy_session: OccupancySessionGeneration::from_uuid(Uuid::from_u128(42)),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: None,
            detached_at: at(0),
            max_resume_duration: Duration::ZERO,
            carbons_enabled: true,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(Show::Away),
            presence_status: Some("back soon".to_owned()),
            presence_priority: 3,
            presence_payloads: Vec::new(),
        },
        inbound_handled,
    }
}

/// Resume with the token of `commit` on a new binding, as alice.
async fn resume(
    store: &DurableSmStore,
    commit: &EnabledCommit,
    new_owner: &BindingRef,
    h: u32,
    now: DateTime<Utc>,
) -> Result<ResumedCommit, ResumeError> {
    let operation =
        ResumeOperation::for_previd(&commit.enabled_reply().id).expect("issued resume token");
    let candidate = store
        .resume_candidate(&operation)
        .await
        .expect("lookup")
        .expect("the token names a session");
    store
        .resume(
            &operation,
            ResumeRequest {
                binding: new_owner,
                presented: &alice(),
                resolved: &candidate.principal,
                standing: PrincipalStanding::Current,
                h,
                claim: None,
                now,
            },
        )
        .await
}

#[tokio::test]
async fn resume_binds_a_new_generation_and_restores_resource_state() {
    let store = store("sm-durable-resume").await;
    let first_owner = binding();
    let commit = enabled(&store, &first_owner).await;
    for body in ["one", "two", "three"] {
        admit_owned(&store, commit.fence(), body).await;
    }
    store
        .detach(commit.fence(), None, snapshot(11), at(10))
        .await
        .expect("detach");

    // A delivery to the detached session is retained, not exposed.
    let detached = store
        .admit(
            AdmissionRequest {
                authority: AdmissionAuthority::Detached {
                    session: commit.fence().session,
                    generation: commit.fence().generation,
                },
                claim: None,
                now: at(11),
            },
            &frame("four"),
        )
        .await
        .expect("detached admission");
    assert_eq!(
        (detached.ordinal(), detached.exposure()),
        (ordinal(4), Exposure::Retained)
    );

    // The client handled the first frame. It cannot have handled the
    // fourth, which was never exposed.
    let second_owner = binding();
    let too_high = resume(&store, &commit, &second_owner, 4, at(20)).await;
    match too_high {
        Err(ResumeError::Rejected(rejection)) => {
            assert!(matches!(
                rejection.reason(),
                ResumeRejectionReason::Denied(ResumeDenial::Handled(HandledError::TooHigh {
                    h: 4,
                    ceiling: 3
                }))
            ));
            assert_eq!(rejection.handled_floor(), Some(11));
            let _permit = rejection.fresh_bind_permit();
        }
        other => panic!("expected a known rejection, got {other:?}"),
    }

    let resumed = resume(&store, &commit, &second_owner, 1, at(20))
        .await
        .expect("resume commits");
    assert_eq!(resumed.fence().generation.value(), 2);
    assert_eq!(resumed.fence().binding, second_owner);
    assert_eq!(resumed.fence().session, commit.fence().session);
    assert_eq!(resumed.full_jid(), &alice_phone());
    assert_eq!(resumed.inbound_handled(), 11);
    assert_eq!(resumed.superseded_binding(), None);
    assert_eq!(
        resumed
            .replay()
            .iter()
            .map(|frame| frame.ordinal)
            .collect::<Vec<_>>(),
        vec![ordinal(2), ordinal(3), ordinal(4)]
    );
    let resource = resumed.resource().expect("detached resource state");
    assert!(resource.carbons_enabled);
    assert_eq!(resource.presence_show, Some(Show::Away));
    assert_eq!(resource.presence_status.as_deref(), Some("back soon"));
    assert_eq!(resource.presence_priority, 3);
    assert_eq!(
        resource.occupancy_session,
        OccupancySessionGeneration::from_uuid(Uuid::from_u128(42))
    );
    assert_eq!(resumed.resumed_reply(11).previd, commit.enabled_reply().id);

    // While hydrating, the new owner's frames are retained above the replay.
    let during = store
        .admit(
            AdmissionRequest {
                authority: AdmissionAuthority::Owner {
                    fence: resumed.fence(),
                    principal: &alice(),
                },
                claim: None,
                now: at(21),
            },
            &frame("five"),
        )
        .await
        .expect("admission during hydration");
    assert_eq!(
        (during.ordinal(), during.exposure()),
        (ordinal(5), Exposure::Retained)
    );

    let tail = store
        .complete_resume(&resumed, None, at(22))
        .await
        .expect("completion");
    assert_eq!(
        tail.frames
            .iter()
            .map(|frame| frame.ordinal)
            .collect::<Vec<_>>(),
        vec![ordinal(5)]
    );
    let stored = store
        .session(commit.fence().session)
        .await
        .expect("read")
        .expect("session");
    assert_eq!(stored.state, DurableSmState::Live);
    assert_eq!(stored.detached_at, None);
    assert_eq!(stored.window.exposure_ceiling(), ordinal(5));
    assert_eq!(
        admit_owned(&store, resumed.fence(), "six").await,
        ordinal(6)
    );
}

#[tokio::test]
async fn a_resume_fences_the_previous_owner_without_its_transport_closing() {
    let store = store("sm-durable-resume-fence").await;
    let first_owner = binding();
    let commit = enabled(&store, &first_owner).await;
    admit_owned(&store, commit.fence(), "one").await;

    // The first owner never detaches: its transport is still open.
    let second_owner = binding();
    let resumed = resume(&store, &commit, &second_owner, 0, at(5))
        .await
        .expect("resume from a live session");
    assert_eq!(resumed.superseded_binding(), Some(&first_owner));

    // Every effect of the superseded owner now fails its generation fence.
    let stale = |error: &FenceError| {
        matches!(error, FenceError::StaleGeneration { stored, presented }
            if stored.value() == 2 && presented.value() == 1)
    };
    match store
        .admit(
            AdmissionRequest {
                authority: AdmissionAuthority::Owner {
                    fence: commit.fence(),
                    principal: &alice(),
                },
                claim: None,
                now: at(6),
            },
            &frame("late"),
        )
        .await
    {
        Err(AdmissionError::Rejected(AdmissionRejection::Fence(error))) => assert!(stale(&error)),
        other => panic!("expected a stale-generation rejection, got {other:?}"),
    }
    match store.settle_ack(commit.fence(), None, 1, at(6)).await {
        Err(OwnerError::Rejected(OwnerRejection::Fence(error))) => assert!(stale(&error)),
        other => panic!("expected a stale-generation rejection, got {other:?}"),
    }
    match store.detach(commit.fence(), None, snapshot(1), at(6)).await {
        Err(OwnerError::Rejected(OwnerRejection::Fence(error))) => assert!(stale(&error)),
        other => panic!("expected a stale-generation rejection, got {other:?}"),
    }
    match store.close(commit.fence(), None, at(6)).await {
        Err(OwnerError::Rejected(OwnerRejection::Fence(error))) => assert!(stale(&error)),
        other => panic!("expected a stale-generation rejection, got {other:?}"),
    }

    let stored = store
        .session(commit.fence().session)
        .await
        .expect("read")
        .expect("session");
    assert_eq!(stored.state, DurableSmState::TransferHydrating);
    assert_eq!(stored.generation.value(), 2);
    assert_eq!(stored.window.appended(), ordinal(1));
}

#[tokio::test]
async fn compensation_is_fenced_by_generation() {
    let store = store("sm-durable-compensate").await;
    let commit = enabled(&store, &binding()).await;
    store
        .detach(commit.fence(), None, snapshot(2), at(10))
        .await
        .expect("detach");

    let first = resume(&store, &commit, &binding(), 0, at(20))
        .await
        .expect("first resume");
    store
        .compensate_resume(&first, None, at(21))
        .await
        .expect("compensation");
    let stored = store
        .session(commit.fence().session)
        .await
        .expect("read")
        .expect("session");
    assert_eq!(stored.state, DurableSmState::Detached);
    // The window started at the first detach and was not renewed.
    assert_eq!(stored.detached_at, Some(at(10)));

    // The session stays resumable, and the resource state is still there.
    let second = resume(&store, &commit, &binding(), 0, at(30))
        .await
        .expect("second resume");
    assert_eq!(second.fence().generation.value(), 3);
    assert!(second.resource().is_some());

    // A late compensation or completion of the first resume changes nothing.
    assert!(matches!(
        store.compensate_resume(&first, None, at(31)).await,
        Err(OwnerError::Rejected(OwnerRejection::Fence(
            FenceError::StaleGeneration { .. }
        )))
    ));
    assert!(matches!(
        store.complete_resume(&first, None, at(31)).await,
        Err(OwnerError::Rejected(OwnerRejection::Fence(
            FenceError::StaleGeneration { .. }
        )))
    ));
    let stored = store
        .session(commit.fence().session)
        .await
        .expect("read")
        .expect("session");
    assert_eq!(stored.state, DurableSmState::TransferHydrating);
    assert_eq!(stored.generation.value(), 3);
}

#[tokio::test]
async fn resume_rejections_are_known_and_leave_the_session_resumable() {
    let store = store("sm-durable-resume-reject").await;
    let commit = enabled(&store, &binding()).await;
    store
        .detach(commit.fence(), None, snapshot(0), at(10))
        .await
        .expect("detach");
    let operation =
        ResumeOperation::for_previd(&commit.enabled_reply().id).expect("issued resume token");
    let candidate = store
        .resume_candidate(&operation)
        .await
        .expect("lookup")
        .expect("candidate");
    assert_eq!(candidate.principal, alice());
    assert_eq!(candidate.full_jid, alice_phone());

    let new_owner = binding();
    let mallory = principal(9, 1, "mallory@example.test");
    let rotated = principal(1, 8, "alice@example.test");
    let cases: [(&AuthenticatedPrincipalRef, PrincipalStanding, ResumeDenial); 4] = [
        (
            &mallory,
            PrincipalStanding::Current,
            ResumeDenial::PrincipalMismatch,
        ),
        (
            &rotated,
            PrincipalStanding::Current,
            ResumeDenial::PrincipalMismatch,
        ),
        (
            &candidate.principal,
            PrincipalStanding::Missing,
            ResumeDenial::PrincipalUnavailable,
        ),
        (
            &candidate.principal,
            PrincipalStanding::Diverged,
            ResumeDenial::PrincipalDiverged,
        ),
    ];
    for (presented, standing, expected) in cases {
        let attempt = ResumeOperation::for_previd(&commit.enabled_reply().id).expect("token");
        let rejected = store
            .resume(
                &attempt,
                ResumeRequest {
                    binding: &new_owner,
                    presented,
                    resolved: &candidate.principal,
                    standing,
                    h: 0,
                    claim: None,
                    now: at(20),
                },
            )
            .await;
        match rejected {
            Err(ResumeError::Rejected(rejection)) => assert!(
                matches!(rejection.reason(), ResumeRejectionReason::Denied(denial) if *denial == expected),
                "expected {expected:?}, got {:?}",
                rejection.reason()
            ),
            other => panic!("expected a known rejection, got {other:?}"),
        }
    }

    // A resume id this server never issued names no session.
    assert!(ResumeOperation::for_previd("not-a-token").is_err());
    let unknown = ResumeOperation::for_previd(
        &waddle_xmpp::stream_management::durable::SmResumeToken::mint().to_wire(),
    )
    .expect("well-formed token");
    assert_eq!(
        store.resume_candidate(&unknown).await.expect("lookup"),
        None
    );

    // An operation resolved before it ran is abandoned and cannot commit.
    let barred = ResumeOperation::for_previd(&commit.enabled_reply().id).expect("token");
    match store
        .resolve_resume(&barred, &new_owner, at(21))
        .await
        .expect("resolution is known")
    {
        ResumeResolution::Rejected(rejection) => {
            assert!(matches!(
                rejection.reason(),
                ResumeRejectionReason::Abandoned
            ));
        }
        ResumeResolution::Committed(_) => panic!("the resume never ran"),
    }
    let request = ResumeRequest {
        binding: &new_owner,
        presented: &candidate.principal,
        resolved: &candidate.principal,
        standing: PrincipalStanding::Current,
        h: 0,
        claim: None,
        now: at(22),
    };
    assert!(matches!(
        store.resume(&barred, request).await,
        Err(ResumeError::Rejected(rejection))
            if matches!(rejection.reason(), ResumeRejectionReason::Abandoned)
    ));

    // None of the rejections changed the session; a valid resume commits,
    // and resolving its operation afterwards reports the commit.
    let resumed = store
        .resume(&operation, request)
        .await
        .expect("valid resume");
    match store
        .resolve_resume(&operation, &new_owner, at(23))
        .await
        .expect("resolution is known")
    {
        ResumeResolution::Committed(resolved) => assert_eq!(resolved.fence(), resumed.fence()),
        ResumeResolution::Rejected(rejection) => panic!("committed resume: {rejection}"),
    }
}

#[tokio::test]
async fn a_detached_session_expires_only_after_its_window_and_keeps_custody() {
    let store = store("sm-durable-expire").await;
    let commit = enabled(&store, &binding()).await;
    let session = commit.fence().session;
    admit_owned(&store, commit.fence(), "one").await;
    store
        .detach(commit.fence(), None, snapshot(0), at(10))
        .await
        .expect("detach");

    assert!(store
        .due_for_expiry(at(309), 10)
        .await
        .expect("scan")
        .is_empty());
    assert!(matches!(
        store.expire(session, None, at(309)).await,
        Err(LifecycleError::Rejected(LifecycleRejection::NotDue))
    ));
    assert_eq!(
        store.due_for_expiry(at(310), 10).await.expect("scan"),
        vec![session]
    );
    let expired = store
        .expire(session, None, at(310))
        .await
        .expect("expiry commits");
    assert_eq!(
        expired.state,
        DurableSmState::Terminal(TerminalReason::Expired)
    );
    assert_eq!(expired.retained_frames, 1);

    // A terminal session cannot be resumed, and the rejection is known.
    match resume(&store, &commit, &binding(), 0, at(311)).await {
        Err(ResumeError::Rejected(rejection)) => assert!(matches!(
            rejection.reason(),
            ResumeRejectionReason::Denied(ResumeDenial::Terminal(TerminalReason::Expired))
        )),
        other => panic!("expected a terminal rejection, got {other:?}"),
    }

    // Its frame stays in custody until the transfer consumer releases it.
    let frames = store.terminal_frames(session).await.expect("custody");
    assert_eq!(
        frames.iter().map(|frame| frame.ordinal).collect::<Vec<_>>(),
        vec![ordinal(1)]
    );
    assert_eq!(store.purge_terminal(at(1_000)).await.expect("purge"), 0);
    store
        .release_terminal(session, None, at(400))
        .await
        .expect("release");
    assert!(store
        .terminal_frames(session)
        .await
        .expect("custody")
        .is_empty());
    assert_eq!(store.purge_terminal(at(1_000)).await.expect("purge"), 1);
    assert_eq!(store.session(session).await.expect("read"), None);
}

#[tokio::test]
async fn a_restart_detaches_owned_sessions_and_a_rebind_replaces_them() {
    let store = store("sm-durable-restart").await;
    let commit = enabled(&store, &binding()).await;
    let session = commit.fence().session;

    assert_eq!(
        store
            .detach_owners_of_previous_process(at(50))
            .await
            .expect("recovery"),
        1
    );
    let stored = store
        .session(session)
        .await
        .expect("read")
        .expect("session");
    assert_eq!(stored.state, DurableSmState::Detached);
    assert_eq!(stored.detached_at, Some(at(50)));

    // The session is resumable after the restart, with no resource snapshot
    // because its owner never detached.
    let resumed = resume(&store, &commit, &binding(), 0, at(60))
        .await
        .expect("resume after restart");
    assert!(resumed.resource().is_none());

    // Binding the resource again ends the session.
    let replaced = store
        .replace_resource_sessions(&alice_phone(), at(70))
        .await
        .expect("replace");
    assert_eq!(replaced.len(), 1);
    assert_eq!(
        replaced[0].state,
        DurableSmState::Terminal(TerminalReason::Replaced)
    );
    assert!(matches!(
        store.complete_resume(&resumed, None, at(71)).await,
        Err(OwnerError::Rejected(OwnerRejection::NotOwned(
            DurableSmState::Terminal(TerminalReason::Replaced)
        )))
    ));
}
