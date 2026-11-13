//! Pre-accept custody and acknowledgement settlement over single-node SQLite.
//!
//! A frame's payload and ordinal commit before a witness exists, the owner
//! fence and principal are compared under the session lock, the retained
//! window is bounded, and a handled count is validated against the durable
//! exposure ceiling. Claim fencing and commit-uncertainty need PostgreSQL
//! fixtures and are not covered.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use jid::FullJid;
use uuid::Uuid;
use waddle_server::db::Database;
use waddle_server::sm_authority::{
    AdmissionAuthority, AdmissionError, AdmissionFrame, AdmissionRejection, AdmissionRequest,
    DurableSmStore, EnableOperation, EnableRequest, EnabledCommit, OwnerError, OwnerRejection,
};
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::stream_management::durable::{
    AppendError, Exposure, ExposureBounds, FenceError, HandledDecision, HandledError, OwnerFence,
    SmOperationId, SmOrdinal,
};
use waddle_xmpp::stream_management::persistence::SmReplayPayload;
use waddle_xmpp::stream_management::SmFrameProvenance;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Lang, Message};

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
    let bounds = ExposureBounds::new(3, 4096).expect("test bounds");
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
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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

fn owner_request<'a>(
    fence: &'a OwnerFence,
    principal: &'a AuthenticatedPrincipalRef,
) -> AdmissionRequest<'a> {
    AdmissionRequest {
        authority: AdmissionAuthority::Owner { fence, principal },
        claim: None,
        now: at(2),
    }
}

#[tokio::test]
async fn admission_commits_the_payload_and_ordinal_before_the_witness() {
    let store = store("sm-durable-admit").await;
    let owner = binding();
    let commit = enabled(&store, &owner).await;
    let alice = alice();
    let (first, second) = (frame("one"), frame("two"));

    let admitted_first = store
        .admit(owner_request(commit.fence(), &alice), &first)
        .await
        .expect("first frame admitted");
    let admitted_second = store
        .admit(owner_request(commit.fence(), &alice), &second)
        .await
        .expect("second frame admitted");

    assert_eq!(admitted_first.ordinal(), ordinal(1));
    assert_eq!(admitted_second.ordinal(), ordinal(2));
    assert_eq!(admitted_second.sequence(), 2);
    assert_eq!(admitted_first.exposure(), Exposure::Exposed);
    assert_eq!(admitted_first.generation(), commit.fence().generation);

    // The stored custody holds the exact bytes, in ordinal order, and the
    // exposure ceiling covers both frames.
    let retained = store
        .retained_frames(commit.fence().session)
        .await
        .expect("retained frames");
    assert_eq!(
        retained
            .iter()
            .map(|retained| (retained.ordinal, retained.frame.stanza.wire().to_owned()))
            .collect::<Vec<_>>(),
        vec![
            (ordinal(1), first.payload.wire().to_owned()),
            (ordinal(2), second.payload.wire().to_owned()),
        ]
    );
    let stored = store
        .session(commit.fence().session)
        .await
        .expect("read")
        .expect("session");
    assert_eq!(stored.window.exposure_ceiling(), ordinal(2));
    assert_eq!(stored.window.appended(), ordinal(2));
    assert_eq!(
        stored.unacked_bytes,
        (first.payload.wire().len() + second.payload.wire().len()) as u64
    );

    // Offering the same operation again returns its first ordinal and
    // stores nothing more.
    let repeated = store
        .admit(owner_request(commit.fence(), &alice), &first)
        .await
        .expect("idempotent admission");
    assert_eq!(repeated, admitted_first);
    assert_eq!(
        store
            .retained_frames(commit.fence().session)
            .await
            .expect("retained frames")
            .len(),
        2
    );
}

#[tokio::test]
async fn admission_requires_the_exact_owner_fence_and_stored_principal() {
    let store = store("sm-durable-admit-fence").await;
    let owner = binding();
    let commit = enabled(&store, &owner).await;
    let alice = alice();

    let other_binding = OwnerFence {
        binding: binding(),
        ..commit.fence().clone()
    };
    assert!(matches!(
        store
            .admit(owner_request(&other_binding, &alice), &frame("stale"))
            .await,
        Err(AdmissionError::Rejected(AdmissionRejection::Fence(
            FenceError::BindingMismatch
        )))
    ));

    // The same account authenticated in another context is not the stored
    // principal reference.
    let other_context = principal(2, 7, "alice@example.test");
    assert!(matches!(
        store
            .admit(owner_request(commit.fence(), &other_context), &frame("x"))
            .await,
        Err(AdmissionError::Rejected(
            AdmissionRejection::PrincipalMismatch
        ))
    ));

    // A detached delivery is refused while a binding owns the session.
    assert!(matches!(
        store
            .admit(
                AdmissionRequest {
                    authority: AdmissionAuthority::Detached {
                        session: commit.fence().session,
                        generation: commit.fence().generation,
                    },
                    claim: None,
                    now: at(2),
                },
                &frame("detached"),
            )
            .await,
        Err(AdmissionError::Rejected(AdmissionRejection::NotAdmitting(
            _
        )))
    ));

    assert!(store
        .retained_frames(commit.fence().session)
        .await
        .expect("retained frames")
        .is_empty());
}

#[tokio::test]
async fn admission_refuses_a_frame_beyond_the_retained_bound() {
    let store = store("sm-durable-admit-bound").await;
    let owner = binding();
    let commit = enabled(&store, &owner).await;
    let alice = alice();
    for body in ["one", "two", "three"] {
        let _ = store
            .admit(owner_request(commit.fence(), &alice), &frame(body))
            .await
            .expect("within the bound");
    }
    assert!(matches!(
        store
            .admit(owner_request(commit.fence(), &alice), &frame("four"))
            .await,
        Err(AdmissionError::Rejected(AdmissionRejection::Bound(
            AppendError::OutstandingBound
        )))
    ));

    // Acknowledging one frame makes room for one more.
    let settled = store
        .settle_ack(commit.fence(), None, 1, at(3))
        .await
        .expect("ack settles");
    assert_eq!(settled.window.outstanding(), 2);
    let admitted = store
        .admit(owner_request(commit.fence(), &alice), &frame("four"))
        .await
        .expect("room after the acknowledgement");
    assert_eq!(admitted.ordinal(), ordinal(4));
}

#[tokio::test]
async fn acknowledgement_is_validated_against_the_exposure_ceiling() {
    let store = store("sm-durable-ack").await;
    let owner = binding();
    let commit = enabled(&store, &owner).await;
    let alice = alice();
    for body in ["one", "two", "three"] {
        let _ = store
            .admit(owner_request(commit.fence(), &alice), &frame(body))
            .await
            .expect("admitted");
    }

    let settled = store
        .settle_ack(commit.fence(), None, 2, at(3))
        .await
        .expect("ack settles");
    assert_eq!(
        settled.decision,
        HandledDecision::Advanced {
            through: ordinal(2),
            released: 2
        }
    );
    let retained = store
        .retained_frames(commit.fence().session)
        .await
        .expect("retained frames");
    assert_eq!(
        retained
            .iter()
            .map(|frame| frame.ordinal)
            .collect::<Vec<_>>(),
        vec![ordinal(3)]
    );

    // A count above what was exposed is an error and releases nothing.
    assert!(matches!(
        store.settle_ack(commit.fence(), None, 4, at(4)).await,
        Err(OwnerError::Rejected(OwnerRejection::Handled(
            HandledError::TooHigh { h: 4, ceiling: 3 }
        )))
    ));
    // An acknowledgement older than the floor is ignored.
    let stale = store
        .settle_ack(commit.fence(), None, 1, at(5))
        .await
        .expect("stale ack is not an error");
    assert_eq!(stale.decision, HandledDecision::IgnoredStale);
    assert_eq!(
        store
            .retained_frames(commit.fence().session)
            .await
            .expect("retained frames")
            .len(),
        1
    );

    // Another binding cannot settle the owner's window.
    let other = OwnerFence {
        binding: binding(),
        ..commit.fence().clone()
    };
    assert!(matches!(
        store.settle_ack(&other, None, 3, at(6)).await,
        Err(OwnerError::Rejected(OwnerRejection::Fence(
            FenceError::BindingMismatch
        )))
    ));
}

#[tokio::test]
async fn the_inbound_handled_count_never_decreases() {
    let store = store("sm-durable-inbound").await;
    let owner = binding();
    let commit = enabled(&store, &owner).await;
    for handled in [5, 3, 9] {
        store
            .record_inbound_handled(commit.fence(), None, handled, at(1))
            .await
            .expect("recorded");
    }
    let stored = store
        .session(commit.fence().session)
        .await
        .expect("read")
        .expect("session");
    assert_eq!(stored.inbound_handled, 9);
}
