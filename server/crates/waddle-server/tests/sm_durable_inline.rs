//! XEP-0198 inline resume and Bind 2 enablement over the durable authority.
//!
//! These cases drive the decision functions a SASL2 success path calls: the
//! order of resume and bind, the inline results each outcome produces, and
//! the Bind 2 fallback after a known resume failure. They run against
//! single-node SQLite and exchange no frames; the SASL2 handshake that would
//! carry these elements is not wired to them.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use jid::FullJid;
use uuid::Uuid;
use waddle_server::db::Database;
use waddle_server::sm_authority::{
    decide_bind2, granted_resume_window, inline_results, plan_inline, Bind2Decision,
    Bind2EnableOutcome, Bind2StreamManagement, DurableSmError, DurableSmStore, EnableOperation,
    EnableRequest, EnabledCommit, InlineOutcomeUnknown, InlinePlan, InlineResumeFailed,
    InlineResumeOutcome, InlineResumeRequest, InlineSession,
};
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::stream_management::durable::{ExposureBounds, PrincipalStanding, SmResumeToken};
use waddle_xmpp_core::xep0388_inline::{
    Bind2Request, Bind2StreamManagementResult, Sasl2InlineRequests, SmInlineEnable, SmInlineResume,
    SmInlineResumeResult, SmStreamId,
};
use xmpp_parsers::stanza_error::DefinedCondition;

fn alice() -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        "alice@example.test".parse().expect("bare jid"),
        AuthContextId::new(Uuid::from_u128(1)),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::new(7),
    )
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
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                claim: None,
                now: at(0),
            },
        )
        .await
        .expect("enable commits")
}

fn resume_request(previd: &str, h: u32) -> SmInlineResume {
    SmInlineResume {
        previd: SmStreamId::new(previd).expect("non-empty previd"),
        h,
    }
}

fn bind_with_enable() -> Bind2Request {
    Bind2Request {
        stream_management: Some(SmInlineEnable {
            resume: true,
            max: Some(120),
        }),
        ..Bind2Request::default()
    }
}

#[test]
fn resume_is_planned_before_bind_and_bind_is_its_fallback() {
    let resume = resume_request("previd", 3);
    let bind = bind_with_enable();
    assert_eq!(
        plan_inline(&Sasl2InlineRequests::default()),
        InlinePlan::None
    );
    assert_eq!(
        plan_inline(&Sasl2InlineRequests {
            resume: None,
            bind: Some(bind.clone()),
        }),
        InlinePlan::Bind(&bind)
    );
    assert_eq!(
        plan_inline(&Sasl2InlineRequests {
            resume: Some(resume.clone()),
            bind: Some(bind.clone()),
        }),
        InlinePlan::Resume {
            resume: &resume,
            fallback: Some(&bind),
        }
    );

    // A planned resume with no outcome yet, or an unknown one, decides no
    // bind: the exchange may neither fall back nor skip.
    let plan = InlinePlan::Resume {
        resume: &resume,
        fallback: Some(&bind),
    };
    assert_eq!(decide_bind2(plan, None), Err(InlineOutcomeUnknown));
    let unknown = InlineResumeOutcome::Unknown(DurableSmError::BoundsUnproven);
    assert_eq!(
        decide_bind2(plan, Some(&unknown)),
        Err(InlineOutcomeUnknown)
    );

    // A known failure falls back to the requested bind and its enable.
    let failed = InlineResumeOutcome::Failed(InlineResumeFailed::standing_unavailable());
    assert_eq!(
        decide_bind2(plan, Some(&failed)),
        Ok(Bind2Decision::Bind {
            request: &bind,
            enable: bind.stream_management,
        })
    );
    assert_eq!(
        decide_bind2(
            InlinePlan::Resume {
                resume: &resume,
                fallback: None
            },
            Some(&failed)
        ),
        Ok(Bind2Decision::NotRequested)
    );
    assert_eq!(
        granted_resume_window(
            SmInlineEnable {
                resume: true,
                max: Some(120)
            },
            Duration::from_secs(300)
        ),
        Duration::from_secs(120)
    );
    assert_eq!(
        granted_resume_window(
            SmInlineEnable {
                resume: true,
                max: Some(900)
            },
            Duration::from_secs(300)
        ),
        Duration::from_secs(300)
    );
}

#[tokio::test]
async fn an_inline_resume_reports_resumed_and_skips_the_bind() {
    let store = store("sm-durable-inline-resume").await;
    let commit = enabled(&store, &binding()).await;
    let previd = commit.enabled_reply().id;
    let requests = Sasl2InlineRequests {
        resume: Some(resume_request(&previd, 0)),
        bind: Some(bind_with_enable()),
    };
    let plan = plan_inline(&requests);
    let InlinePlan::Resume { resume, .. } = plan else {
        panic!("a resume was requested");
    };

    let attempt = store
        .begin_inline_resume(resume)
        .await
        .expect("the previd names a session");
    assert_eq!(attempt.stored_principal(), &alice());
    assert_eq!(attempt.full_jid(), &alice_phone());
    let new_owner = binding();
    let outcome = store
        .finish_inline_resume(
            &attempt,
            InlineResumeRequest {
                binding: &new_owner,
                presented: &alice(),
                standing: PrincipalStanding::Current,
                claim: None,
                now: at(5),
            },
        )
        .await;
    assert_eq!(
        decide_bind2(plan, Some(&outcome)),
        Ok(Bind2Decision::SkippedForResume)
    );
    let InlineResumeOutcome::Resumed(resumed) = outcome else {
        panic!("expected a committed resume, got {outcome:?}");
    };
    assert_eq!(resumed.fence().binding, new_owner);

    let results = inline_results(InlineSession::Resumed {
        commit: &resumed,
        h: 9,
    })
    .expect("inline results");
    match results.resume {
        Some(SmInlineResumeResult::Resumed(inline)) => {
            assert_eq!(inline.previd.as_str(), previd);
            assert_eq!(inline.h, 9);
        }
        other => panic!("expected <resumed/>, got {other:?}"),
    }
    assert_eq!(results.bound, None);
}

#[tokio::test]
async fn a_failed_inline_resume_reports_failed_and_falls_back_to_bind2() {
    let store = store("sm-durable-inline-fallback").await;
    let bind = bind_with_enable();

    // A previd this server never issued, and a well-formed token that names
    // no session, both fail as item-not-found.
    for previd in ["legacy-stream-id", &SmResumeToken::mint().to_wire()] {
        let failed = store
            .begin_inline_resume(&resume_request(previd, 0))
            .await
            .expect_err("no such session");
        assert_eq!(
            failed.failed().condition,
            Some(DefinedCondition::ItemNotFound)
        );
        assert_eq!(failed.failed().h, None);
    }

    // A session whose stored principal no longer resolves is not authorized
    // and reports the server's handled count.
    let commit = enabled(&store, &binding()).await;
    store
        .record_inbound_handled(commit.fence(), None, 4, at(1))
        .await
        .expect("recorded");
    let resume = resume_request(&commit.enabled_reply().id, 0);
    let attempt = store
        .begin_inline_resume(&resume)
        .await
        .expect("the previd names a session");
    let new_owner = binding();
    let outcome = store
        .finish_inline_resume(
            &attempt,
            InlineResumeRequest {
                binding: &new_owner,
                presented: &alice(),
                standing: PrincipalStanding::Missing,
                claim: None,
                now: at(5),
            },
        )
        .await;
    let plan = InlinePlan::Resume {
        resume: &resume,
        fallback: Some(&bind),
    };
    let decision = decide_bind2(plan, Some(&outcome)).expect("a known outcome");
    let InlineResumeOutcome::Failed(failed) = &outcome else {
        panic!("expected a known failure, got {outcome:?}");
    };
    assert_eq!(
        failed.failed().condition,
        Some(DefinedCondition::NotAuthorized)
    );
    assert_eq!(failed.failed().h, Some(4));
    let _permit = failed.fresh_bind_permit();
    let Bind2Decision::Bind { enable, .. } = decision else {
        panic!("expected the fallback bind, got {decision:?}");
    };
    let enable = enable.expect("the bind asked for stream management");

    // The fallback binds a fresh resource and enables a new session for it.
    let operation = EnableOperation::mint();
    let tablet: FullJid = "alice@example.test/tablet".parse().expect("full jid");
    let enabled = store
        .inline_enable(
            &operation,
            EnableRequest {
                binding: &new_owner,
                principal: &alice(),
                full_jid: &tablet,
                max_resume: granted_resume_window(enable, Duration::from_secs(300)),
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                claim: None,
                now: at(6),
            },
        )
        .await;
    let Bind2EnableOutcome::Enabled(new_commit) = enabled else {
        panic!("expected a committed enable, got {enabled:?}");
    };
    let results = inline_results(InlineSession::Bound {
        resume_failed: Some(failed),
        stream_management: Some(Bind2StreamManagement::Enabled {
            commit: &new_commit,
            resumable: enable.resume,
        }),
    })
    .expect("inline results");
    assert!(matches!(
        results.resume,
        Some(SmInlineResumeResult::Failed(_))
    ));
    let bound = results.bound.expect("<bound/>");
    match bound.stream_management {
        Some(Bind2StreamManagementResult::Enabled(inline)) => {
            assert!(inline.resume);
            assert_eq!(inline.max, Some(120));
            assert_eq!(
                inline.id.as_ref().map(SmStreamId::as_str),
                Some(new_commit.enabled_reply().id.as_str())
            );
        }
        other => panic!("expected <enabled/>, got {other:?}"),
    }

    // A client that did not ask for resumption is given no resume id.
    let results = inline_results(InlineSession::Bound {
        resume_failed: None,
        stream_management: Some(Bind2StreamManagement::Enabled {
            commit: &new_commit,
            resumable: false,
        }),
    })
    .expect("inline results");
    match results.bound.and_then(|bound| bound.stream_management) {
        Some(Bind2StreamManagementResult::Enabled(inline)) => {
            assert!(!inline.resume);
            assert_eq!(inline.id, None);
        }
        other => panic!("expected <enabled/>, got {other:?}"),
    }

    // The original session is untouched by the failed resume.
    let stored = store
        .session(commit.fence().session)
        .await
        .expect("read")
        .expect("session");
    assert_eq!(stored.generation.value(), 1);
}

#[tokio::test]
async fn an_inline_resume_of_an_ended_session_is_a_known_item_not_found() {
    let store = store("sm-durable-inline-terminal").await;
    let commit = enabled(&store, &binding()).await;
    let resume = resume_request(&commit.enabled_reply().id, 0);
    store
        .close(commit.fence(), None, at(2))
        .await
        .expect("the owner closes its session");

    // The session is refused when it is looked up, before the caller takes
    // a claim or reads an ingress checkpoint that an ended session no
    // longer has, and the known failure lets the exchange fall back to its
    // bind.
    let failed = store
        .begin_inline_resume(&resume)
        .await
        .expect_err("an ended session cannot be resumed");
    assert_eq!(
        failed.failed().condition,
        Some(DefinedCondition::ItemNotFound)
    );
    let bind = bind_with_enable();
    let plan = InlinePlan::Resume {
        resume: &resume,
        fallback: Some(&bind),
    };
    assert!(matches!(
        decide_bind2(plan, Some(&InlineResumeOutcome::Failed(failed))),
        Ok(Bind2Decision::Bind { .. })
    ));
}
