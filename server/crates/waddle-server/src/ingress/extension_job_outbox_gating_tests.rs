//! End-to-end coverage of `apply_durable`'s `extension_job_outbox`
//! grant-derived enqueue gate (`ingress/durable.rs`'s
//! `tx.extension_manager()` block, issue #1831 Phase B). Mirrors the
//! retired `judgment_outbox_gating_tests` (config-flag era) test-for-test,
//! swapped onto the new mechanism: whether a row is enqueued now depends on
//! whether a loaded/granted extension currently holds the `durable-job`
//! grant for the `message-judge` job kind, asked live of a real
//! `ExtensionManager` — never a static config flag.
//!
//! These tests drive a real `commit_submission` call — the same entry
//! point production ingress uses.
use super::commit::commit_submission;
use super::test_support::IngressFixture;
use super::{IngressPrincipal, IngressStreamIdentity, IngressSubmission, PlannedEffect};
use crate::server::routes::interpret::effects::{
    direct::DurableDirectEffect,
    room::{DurableRoomEffect, RoomFenceRequirement},
    DurableEffect, Effect, IngressPlan, RoomExecutionPath,
};
use jid::{BareJid, Jid};
use std::sync::Arc;
use waddle_extensions::{
    ExtensionCapability, ExtensionConfig, ExtensionManager, ExtensionModuleConfig,
};
use waddle_xmpp::ingress::{
    ConnectionGeneration, DigestContext, DigestInput, IngressEffectIntent, NormalizedTarget,
    TransportGeneration,
};
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::{Lang, Message, MessageType};

fn message_hook_fixture_path() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../waddle-extensions/tests/fixtures/message_hook.wasm")
        .to_string_lossy()
        .into_owned()
}

/// An `ExtensionManager` with one loaded actor holding the `durable-job`
/// grant for the `message-judge` job kind — the live state
/// `ExtensionManager::durable_job_grant_holder` (and therefore the enqueue
/// gate) must find.
async fn manager_with_granted_judge() -> Arc<ExtensionManager> {
    let manager = ExtensionManager::from_config(ExtensionConfig {
        enabled: true,
        modules: vec![ExtensionModuleConfig {
            name: "message-hook-fixture".into(),
            namespace: "urn:test:message-hook".into(),
            registry: Default::default(),
            digest: None,
            tag: None,
            config: serde_json::json!({"capabilities": [15], "job_kinds": ["message-judge"]}),
            capability_grants: vec![ExtensionCapability::DurableJob],
            allowed_http_origins: vec![],
            provider_room_grants: vec![],
            config_secret_files: Default::default(),
            local_path: Some(message_hook_fixture_path()),
        }],
        ..ExtensionConfig::default()
    })
    .await
    .expect("granted judge fixture must load");
    Arc::new(manager)
}

/// Schema-initialized fixture whose unit of work is wired to a manager that
/// currently grants `message-judge`. Every test that needs the gate open
/// builds from this; the "gate closed" tests below build their own fixture
/// instead and either skip `set_extension_manager` entirely or use a
/// manager that does not grant the job.
async fn enabled_fixture() -> IngressFixture {
    let mut fixture = IngressFixture::sqlite().await;
    crate::extension_job_outbox::initialize(&fixture.db)
        .await
        .expect("extension job outbox schema");
    fixture
        .uow
        .set_extension_manager(manager_with_granted_judge().await);
    fixture
}

async fn row_count(fixture: &IngressFixture) -> i64 {
    fixture.count("extension_job_outbox").await
}

/// Append one `DurableDirectEffect::ArchiveDirect` planned effect (plus its
/// matching `ArchiveAuthoritative` intent) to `submission`, archiving
/// `body` under `archive` as if `from` sent it.
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

#[tokio::test]
async fn direct_message_enqueues_exactly_one_row_not_two() {
    let fixture = enabled_fixture().await;
    let mut submission = fixture.submission(Some("job-direct"), "hello there");
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
        "only the sender's own archive copy enqueues a job row, not the recipient's too"
    );
    fixture.close().await;
}

#[tokio::test]
async fn groupchat_message_enqueues_exactly_one_row() {
    let fixture = enabled_fixture().await;
    let room: BareJid = "waddlers@muc.example.com".parse().expect("room");
    let mut submission = groupchat_submission(&fixture, &room, "job-groupchat", "hello room");
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
    let mut submission = fixture.submission(Some("job-retransmit"), "hello there");
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
async fn empty_and_whitespace_only_bodies_enqueue_zero_rows() {
    let fixture = enabled_fixture().await;
    let sender = fixture.principal.bare_jid().clone();
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    for (origin, id, body) in [
        ("job-empty-body", "empty-body-copy", ""),
        ("job-whitespace-body", "whitespace-body-copy", "   \t\n"),
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
        "an empty or whitespace-only body must never enqueue a job row"
    );
    fixture.close().await;
}

#[tokio::test]
async fn no_extension_manager_configured_never_enqueues_a_row() {
    // Deliberately does not call `set_extension_manager` -- the default,
    // every-test-construction-unless-opted-in state.
    let fixture = IngressFixture::sqlite().await;
    crate::extension_job_outbox::initialize(&fixture.db)
        .await
        .expect("extension job outbox schema");
    let mut submission = fixture.submission(Some("job-no-manager"), "hello there");
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

    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    assert!(decision.class.advances());
    assert_eq!(
        row_count(&fixture).await,
        0,
        "with no extension manager configured, nothing must ever be enqueued"
    );
    fixture.close().await;
}

#[tokio::test]
async fn extension_manager_present_but_grant_not_held_never_enqueues_a_row() {
    // A live manager IS configured and its one module loads successfully
    // (it declares and is granted `message.observe`, so
    // `validate_manifest_against_module` accepts it), but that module does
    // NOT declare the `durable-job` capability at all -- e.g. the operator
    // grants it a different capability than the one `message-judge` needs.
    // This is the only way a real `ExtensionManager` can end up with a
    // loaded module that is not a `durable-job` grant holder:
    // `ExtensionManager::from_config`'s `validate_manifest_against_module`
    // refuses to load ANY module that declares a capability without a
    // matching operator grant (declaring-but-ungranted is a hard load
    // failure, not a partial/degraded load), so "loaded but this exact
    // grant isn't held" can only mean "never declared it in the first
    // place" -- which is exactly the live state the grant-derived gate
    // must react to.
    let mut fixture = IngressFixture::sqlite().await;
    crate::extension_job_outbox::initialize(&fixture.db)
        .await
        .expect("extension job outbox schema");
    let ungranted_manager = ExtensionManager::from_config(ExtensionConfig {
        enabled: true,
        modules: vec![ExtensionModuleConfig {
            name: "message-hook-fixture".into(),
            namespace: "urn:test:message-hook".into(),
            registry: Default::default(),
            digest: None,
            tag: None,
            // Ordinal 1 = MessageObserve, never 15 (DurableJob) -- this
            // module simply does not participate in durable jobs at all.
            config: serde_json::json!({"capabilities": [1]}),
            capability_grants: vec![ExtensionCapability::MessageObserve],
            allowed_http_origins: vec![],
            provider_room_grants: vec![],
            config_secret_files: Default::default(),
            local_path: Some(message_hook_fixture_path()),
        }],
        ..ExtensionConfig::default()
    })
    .await
    .expect("fixture granted an unrelated capability must still load");
    fixture
        .uow
        .set_extension_manager(Arc::new(ungranted_manager));

    let mut submission = fixture.submission(Some("job-ungranted"), "hello there");
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

    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    assert!(decision.class.advances());
    assert_eq!(
        row_count(&fixture).await,
        0,
        "a manager with no extension currently holding the grant must never enqueue"
    );
    fixture.close().await;
}
