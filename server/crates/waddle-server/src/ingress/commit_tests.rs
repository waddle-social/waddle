use super::*;
use crate::server::routes::interpret::effects::{IngressPlan, RoomExecutionPath};
use crate::{
    config::LineageConfig,
    db::{lineage, Database, DatabaseConfig, DatabaseDriver, MigrationRunner},
};
use waddle_xmpp::{
    auth::{AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch},
    ingress::{ConnectionGeneration, DigestContext, DigestInput, NormalizedTarget},
};

async fn fixture() -> (Database, IngressUnitOfWork, IngressSubmission) {
    let db = Database::from_config(
        "ingress-commit",
        &DatabaseConfig::new(DatabaseDriver::Sqlite, ":memory:"),
    )
    .await
    .expect("database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    let lineage = LineageConfig {
        deployment_uuid: Some(lineage::DeploymentUuid(uuid::Uuid::new_v4())),
        action: None,
    };
    lineage::enroll(&db, &lineage).await.expect("lineage");
    let uow = IngressUnitOfWork::open(db.clone(), lineage).expect("uow");
    let principal = AuthenticatedPrincipalRef::new(
        "romeo@example.com".parse().expect("sender"),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::new(1),
        PrincipalAuthEpoch::new(1),
    );
    let conn = db.guard().await.expect("guard");
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute("INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)", crate::db_params![principal.bare_jid().to_string(), "romeo".to_owned(), "romeo".to_owned(), now.clone(), now.clone()]).await.expect("user");
    conn.execute("INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)", crate::db_params!["session".to_owned(), principal.bare_jid().to_string(), "token".to_owned(), principal.auth_context_id().as_uuid().to_string(), 1_i64, 1_i64, now.clone(), now]).await.expect("session");
    drop(conn);
    let target = NormalizedTarget::Bare("juliet@example.com".parse().expect("target"));
    let mut message =
        xmpp_parsers::message::Message::new(Some("juliet@example.com".parse().expect("to")));
    message.from = Some("romeo@example.com/phone".parse().expect("from"));
    let digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: target.clone(),
            server_authorities: vec![principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let submission = IngressSubmission {
        sender: "romeo@example.com/phone".parse().expect("sender"),
        identity: IngressStreamIdentity::Ephemeral {
            principal: principal.clone(),
        },
        principal,
        target,
        plan: IngressPlan {
            plan: Vec::new(),
            intents: Vec::new(),
            sanitized_message: message,
            rejection: None,
            error_reply: None,
            room_execution: RoomExecutionPath::None,
        },
        digest_input,
        connection_generation: ConnectionGeneration::INITIAL,
    };
    (db, uow, submission)
}
async fn messages(db: &Database) -> i64 {
    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM ingress_messages", ())
        .await
        .expect("count");
    rows.next()
        .await
        .expect("row")
        .expect("count row")
        .get(0)
        .expect("integer")
}
#[tokio::test]
async fn serialization_exhaustion_rolls_back_every_attempt() {
    let (db, uow, submission) = fixture().await;
    let result = commit_hooks::SERIALIZATION_FAILURES
        .scope(
            std::cell::Cell::new(5),
            commit_submission(&uow, &submission, 3),
        )
        .await;
    assert_eq!(
        result.expect_err("exhausted").class(),
        IngressDecisionClass::SerializationExhaustion
    );
    assert_eq!(messages(&db).await, 0);
}
#[tokio::test]
async fn serialization_retry_reuses_plan_with_fresh_transactions() {
    let (db, uow, submission) = fixture().await;
    let result = commit_hooks::SERIALIZATION_FAILURES
        .scope(
            std::cell::Cell::new(2),
            commit_submission(&uow, &submission, 3),
        )
        .await
        .expect("third commit");
    assert_eq!(result.class, IngressDecisionClass::Accepted);
    assert_eq!(messages(&db).await, 1);
}
#[tokio::test]
async fn ambiguous_commit_preserves_committed_row_without_retrying() {
    let (db, uow, submission) = fixture().await;
    let result = commit_hooks::AMBIGUOUS_COMMIT
        .scope(true, commit_submission(&uow, &submission, 3))
        .await;
    assert_eq!(
        result.expect_err("ambiguous").class(),
        IngressDecisionClass::AmbiguousCommit
    );
    assert_eq!(messages(&db).await, 1);
}
#[test]
fn typed_failure_matrix_is_non_advancing() {
    for (error, class) in [
        (
            IngressUowError::PrincipalAssertionFailed,
            IngressDecisionClass::PrincipalMissing,
        ),
        (
            IngressUowError::RoomGenerationStale,
            IngressDecisionClass::RoomGenerationStale,
        ),
        (
            IngressUowError::IngressFrontierStale,
            IngressDecisionClass::FrontierStale,
        ),
        (
            IngressUowError::AmbiguousCommit,
            IngressDecisionClass::AmbiguousCommit,
        ),
        (
            IngressUowError::EffectIntentConflict,
            IngressDecisionClass::IntentContradiction,
        ),
    ] {
        assert_eq!(classify_failure(&error), class);
        assert!(!class.advances());
    }
}
#[tokio::test]
async fn local_unfenced_room_commits_without_snapshot_revision_proof() {
    let (db, uow, mut submission) = fixture().await;
    submission.plan.room_execution = RoomExecutionPath::Local {
        room: "room@muc.example.com".parse().expect("room"),
        fence: crate::server::routes::interpret::effects::room::RoomFenceRequirement::Unfenced,
        snapshot_generation: 1,
    };
    let decision = commit_submission(&uow, &submission, 3)
        .await
        .expect("single-node room commit");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(messages(&db).await, 1);
}
#[tokio::test]
async fn precommit_timeout_and_frontier_stale_leave_no_rows() {
    for (error, class) in [
        (IngressUowError::Timeout, IngressDecisionClass::Timeout),
        (
            IngressUowError::IngressFrontierStale,
            IngressDecisionClass::FrontierStale,
        ),
    ] {
        let (db, uow, submission) = fixture().await;
        let failure = commit_hooks::FAILURE
            .scope(
                std::cell::RefCell::new(Some(error)),
                commit_submission(&uow, &submission, 3),
            )
            .await
            .expect_err("injected typed failure");
        assert_eq!(failure.class(), class);
        let conn = db.guard().await.expect("guard");
        for table in [
            "ingress_messages",
            "ingress_origin_aliases",
            "ingress_sm_refs",
            "ingress_effect_intents",
            "ingress_effect_receipts",
        ] {
            let mut rows = conn
                .query(&format!("SELECT COUNT(*) FROM {table}"), ())
                .await
                .expect("count");
            let count: i64 = rows
                .next()
                .await
                .expect("row")
                .expect("count row")
                .get(0)
                .expect("integer");
            assert_eq!(count, 0, "{table}");
        }
    }
}

#[tokio::test]
async fn plan_rejection_is_authoritative_for_committed_denials() {
    use crate::server::routes::interpret::effects::{
        AuthorizationDeniedReason, PlanRejection, PolicyDeniedReason, SemanticMalformedReason,
    };
    use waddle_xmpp::ingress::{FrozenStanzaError, FrozenStanzaErrorType, IngressEffectIntent};
    for (reason, class) in [
        (
            PlanRejection::AuthorizationDenied(AuthorizationDeniedReason::BlockedSender),
            IngressDecisionClass::AuthorizationDenied,
        ),
        (
            PlanRejection::PolicyDenied(PolicyDeniedReason::OperationalFenceLoss),
            IngressDecisionClass::PolicyDenied,
        ),
        (
            PlanRejection::SemanticMalformed(SemanticMalformedReason::MalformedPayload),
            IngressDecisionClass::SemanticMalformed,
        ),
        (
            PlanRejection::PolicyDenied(PolicyDeniedReason::CaptureOverflow),
            IngressDecisionClass::CaptureOverflow,
        ),
    ] {
        let (db, uow, mut submission) = fixture().await;
        // Deliberately use one error condition for all classes: the typed reason
        // determines the committed class, not a second classification of XML.
        let error = FrozenStanzaError::new(
            FrozenStanzaErrorType::Cancel,
            waddle_xmpp::StanzaErrorCondition::Forbidden,
        );
        let recipient = "romeo@example.com/phone".parse().expect("recipient");
        let mut reply = submission.plan.sanitized_message.clone();
        reply.type_ = xmpp_parsers::message::MessageType::Error;
        reply.payloads.push(error.to_xmpp().into());
        submission.plan.rejection = Some(reason);
        submission.plan.error_reply = Some(waddle_xmpp::Stanza::Message(reply));
        submission
            .plan
            .intents
            .push(IngressEffectIntent::ErrorReply { recipient, error });
        let decision = commit_submission(&uow, &submission, 3)
            .await
            .expect("committed denial");
        assert_eq!(decision.class, class);
        assert_eq!(messages(&db).await, 1);
    }
}

#[test]
fn missing_archive_guard_checks_generated_authorities_individually() {
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    let sender = IngressEffectIntent::ArchiveAuthoritative {
        archive: room.clone(),
        by: room.clone(),
        stanza_id: waddle_xmpp_core::xep0359::StanzaId::new("sender", room.clone().into()),
        archived_at: chrono::Utc::now(),
    };
    let generated = IngressEffectIntent::SystemMessageArchive {
        sequence: 0,
        archive: room.clone(),
        by: room.clone(),
        stanza_id: waddle_xmpp_core::xep0359::StanzaId::new("generated", room.into()),
        archived_at: chrono::Utc::now(),
    };
    assert!(!missing_planned_archive_authority(&[], &[]));
    assert!(missing_planned_archive_authority(
        std::slice::from_ref(&generated),
        &[]
    ));
    let planned = [sender.clone(), generated.clone()];
    assert!(missing_planned_archive_authority(&planned, &[sender]));
    assert!(!missing_planned_archive_authority(&planned, &planned));
    assert!(!missing_planned_archive_authority(
        std::slice::from_ref(&generated),
        std::slice::from_ref(&generated)
    ));
}

#[tokio::test]
async fn missing_room_stanza_id_is_storage_failure_without_commit() {
    let (db, uow, mut submission) = fixture().await;
    submission.plan.rejection =
        Some(crate::server::routes::interpret::effects::PlanRejection::MissingRoomStanzaId);
    let failure = commit_submission(&uow, &submission, 1)
        .await
        .expect_err("missing room identity must fail closed");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(matches!(
        failure.source,
        IngressUowError::MissingRoomStanzaId
    ));
    assert_eq!(messages(&db).await, 0);
}
