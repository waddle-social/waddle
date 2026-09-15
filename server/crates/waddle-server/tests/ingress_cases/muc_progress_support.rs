//! Public Phase B/C wire regressions; real planner coverage lives in muc_occupant_progress_tests.
use crate::{detached_progress_support as detached, ingress_support::IngressFixture};
use jid::{BareJid, FullJid};
use std::{sync::Arc, time::Duration};
use waddle_server::ingress::{
    commit::commit_submission,
    effects::{delivery::ExternalDeliveryEffect, Effect},
    ExternalEffect, IngressSubmission, PlanSuppressionPolicy, PlannedEffect, RecoveryEnvironment,
};
use waddle_xmpp::xep::xep0421::{self, OccupantId};
use waddle_xmpp::{
    ingress::{
        DigestContext, DigestInput, EffectMessageIdentity, EntityGeneration, IngressEffectIntent,
        NormalizedTarget, StoredMessagePayload,
    },
    registry::ConnectionRegistry,
    stream_management::InMemorySmSessionRegistry,
    Stanza,
};
use waddle_xmpp_core::xep0359::{add_stanza_id, extract_stanza_ids, StanzaId};
use xmpp_parsers::message::{Message, MessageType};

fn room() -> BareJid {
    "room@muc.example.com".parse().expect("room")
}
fn stamp() -> StanzaId {
    StanzaId::new("frozen-room-id", room().into())
}
fn occupant_id() -> waddle_xmpp::xep::xep0421::OccupantId {
    waddle_xmpp::xep::xep0421::OccupantId("opaque-occupant".into())
}

fn make_submission(fixture: &IngressFixture, system: bool) -> IngressSubmission {
    let mut submission = fixture.submission(Some("muc-wire-retry"), "frozen content");
    submission.target = NormalizedTarget::Bare(room());
    submission.plan.sanitized_message.type_ = MessageType::Groupchat;
    submission.plan.sanitized_message.to = Some(room().into());
    refresh_digest(&mut submission);
    let mut source = submission.plan.sanitized_message.clone();
    source.from = Some(if system {
        room().into()
    } else {
        room()
            .with_resource_str("original-nick")
            .expect("nick")
            .into()
    });
    add_stanza_id(&mut source, &stamp());
    if !system {
        waddle_xmpp::xep::xep0421::set_occupant_id_on_message(&mut source, &occupant_id());
    }
    let [a, b, _] = detached::resources();
    let occupants = vec![a, b, submission.sender.clone()];
    let identity = EffectMessageIdentity::StanzaId(stamp());
    submission.plan.intents = vec![if system {
        IngressEffectIntent::RouteMucSystemBroadcast {
            room: room(),
            occupants,
            room_generation: EntityGeneration::INITIAL,
            route_identity: identity,
            system_message: Some(
                StoredMessagePayload::new(source.clone()).expect("system payload"),
            ),
        }
    } else {
        IngressEffectIntent::RouteMucGroupchat {
            room: room(),
            occupants,
            reflection: submission.sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: identity,
        }
    }];
    if !system {
        submission.plan.sanitized_message.from = source.from.clone();
    }
    submission.plan.room_canonical_message = Some(Box::new(source.clone()));
    copies(&mut submission, &source);
    submission
}

fn refresh_digest(submission: &mut IngressSubmission) {
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room()],
            stanza_lang: None,
        },
    )
    .expect("digest");
}

fn copies(submission: &mut IngressSubmission, source: &Message) {
    let [a, b, _] = detached::resources();
    submission.plan.plan = [a, b, submission.sender.clone()]
        .into_iter()
        .map(|resource| {
            let mut message = source.clone();
            message.to = Some(resource.clone().into());
            let policy = if resource == submission.sender {
                PlanSuppressionPolicy::Always
            } else {
                PlanSuppressionPolicy::SenderOnly
            };
            PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::QueueDetached {
                    route_identity: None,
                    call_setup: None,
                    bare: resource.to_bare(),
                    resources: vec![resource],
                    stanza: Box::new(Stanza::Message(message)),
                },
            )))
            .with_suppression(policy)
        })
        .collect();
}

async fn wire(sm: &InMemorySmSessionRegistry, recipient: &FullJid) -> Message {
    let session = detached::queued(sm, recipient).await;
    assert_eq!(
        session.unacked_stanzas.len(),
        1,
        "exactly one occupant copy"
    );
    let element: minidom::Element = session.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("wire XML");
    Message::try_from(element).expect("wire message")
}

fn assert_wire(message: &Message, recipient: &FullJid, system: bool) {
    assert_eq!(message.type_, MessageType::Groupchat);
    assert_eq!(message.to, Some(recipient.clone().into()));
    assert_eq!(
        message.from,
        Some(if system {
            room().into()
        } else {
            room()
                .with_resource_str("original-nick")
                .expect("room nick")
                .into()
        })
    );
    assert_eq!(
        extract_stanza_ids(message),
        vec![stamp()],
        "one frozen XEP-0359 room authority"
    );
    if !system {
        assert_eq!(
            waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(message),
            Some(occupant_id())
        );
    }
    assert_eq!(
        message
            .bodies
            .get(&xmpp_parsers::message::Lang::new())
            .map(String::as_str),
        Some("frozen content")
    );
    let element: minidom::Element = message.clone().into();
    let mut xml = Vec::new();
    element.write_to(&mut xml).expect("transport encoding");
    assert!(
        !String::from_utf8(xml)
            .expect("UTF-8")
            .contains("romeo@example.com"),
        "anonymous occupant copy cannot leak the real sender anywhere"
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OccupantCase {
    Existing,
    Present,
    Missing,
}

fn set_source(submission: &mut IngressSubmission, source: Message) {
    for intent in &mut submission.plan.intents {
        if let IngressEffectIntent::RouteMucSystemBroadcast { system_message, .. } = intent {
            *system_message =
                Some(StoredMessagePayload::new(source.clone()).expect("system source"));
        }
    }
    copies(submission, &source);
    submission.plan.room_canonical_message = Some(Box::new(source));
}

fn occupant_element(message: &Message) -> &minidom::Element {
    let occupants: Vec<_> = message
        .payloads
        .iter()
        .filter(|element| element.is("occupant-id", xep0421::NS_OCCUPANT_ID))
        .collect();
    assert_eq!(
        occupants.len(),
        1,
        "one XEP-0421 occupant-id on queued wire"
    );
    occupants[0]
}

fn assert_same_occupant(original: &Message, rebuilt: &Message) {
    let original = occupant_element(original);
    let rebuilt = occupant_element(rebuilt);
    assert_eq!(original, rebuilt, "frozen occupant-id element is preserved");
    assert_eq!(
        original.attr("id").expect("original id").as_bytes(),
        rebuilt.attr("id").expect("rebuilt id").as_bytes(),
        "opaque occupant-id bytes are preserved"
    );
}

pub async fn occupant_replay(fixture: IngressFixture, system: bool) {
    replay_case(fixture, system, false, false, OccupantCase::Present).await;
}

pub async fn missing_occupant_replay(fixture: IngressFixture) {
    replay_case(fixture, false, false, false, OccupantCase::Missing).await;
}

pub async fn replay(fixture: IngressFixture, system: bool, reconnect: bool, old_row: bool) {
    replay_case(fixture, system, reconnect, old_row, OccupantCase::Existing).await;
}

async fn replay_case(
    fixture: IngressFixture,
    system: bool,
    reconnect: bool,
    old_row: bool,
    occupant_case: OccupantCase,
) {
    let sm = detached::registry(&fixture).await;
    let connections = ConnectionRegistry::new();
    let [a, b, _] = detached::resources();
    let mut submission = make_submission(&fixture, system);
    if occupant_case != OccupantCase::Existing {
        let mut source = *submission
            .plan
            .room_canonical_message
            .clone()
            .expect("source");
        if occupant_case == OccupantCase::Missing {
            xep0421::strip_occupant_id_from_message(&mut source);
        } else {
            xep0421::set_occupant_id_on_message(&mut source, &occupant_id());
        }
        set_source(&mut submission, source);
        if occupant_case == OccupantCase::Missing {
            submission.plan.plan.clear();
        }
    }
    detached::attach(&sm, &a).await;
    detached::attach(&sm, &submission.sender).await;
    if old_row {
        submission.plan.sanitized_message.from = Some(submission.sender.clone().into());
        submission.plan.room_canonical_message = None;
        submission.plan.plan.clear();
    }
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("accept");
    if occupant_case == OccupantCase::Missing {
        let mut tx = fixture.uow.begin().await.expect("inspect frozen source");
        let envelope = waddle_server::ingress_uow::CanonicalMessageRepository::load_envelope(
            &mut tx,
            first.message_key.expect("accepted message key"),
        )
        .await
        .expect("stored envelope")
        .expect("frozen source");
        assert_eq!(envelope.message().type_, MessageType::Groupchat);
        assert_eq!(
            envelope.message().from,
            Some(
                room()
                    .with_resource_str("original-nick")
                    .expect("nick")
                    .into()
            )
        );
        assert_eq!(extract_stanza_ids(envelope.message()), vec![stamp()]);
        assert_eq!(
            xep0421::extract_occupant_id_from_message(envelope.message()),
            None,
            "occupant-id is the only missing provenance component"
        );
        tx.commit().await.expect("finish source inspection");
    }
    detached::execute(&fixture, &first, &connections, &sm).await;
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NULL")
            .await,
        1
    );
    let original = if !old_row && occupant_case != OccupantCase::Missing {
        let message = wire(&sm, &a).await;
        assert_wire(&message, &a, system);
        if occupant_case == OccupantCase::Present {
            assert_eq!(
                xep0421::extract_occupant_id_from_message(&message),
                Some(occupant_id())
            );
        }
        Some(message)
    } else {
        None
    };
    if reconnect {
        submission.sender = "romeo@example.com/new".parse().expect("rejoined sender");
        refresh_digest(&mut submission);
        detached::attach(&sm, &submission.sender).await;
    }
    detached::attach(&sm, &b).await;
    let mut source = make_submission(&fixture, system)
        .plan
        .room_canonical_message
        .expect("canonical source");
    source
        .bodies
        .insert(Default::default(), "provisional retry content".into());
    if !system {
        source.from = Some(
            room()
                .with_resource_str("rejoined-nick")
                .expect("fresh nick")
                .into(),
        );
    }
    submission.plan.sanitized_message.from = source.from.clone();
    if occupant_case != OccupantCase::Existing {
        let provisional = OccupantId("provisional-retry-occupant".into());
        assert_ne!(provisional, occupant_id());
        xep0421::set_occupant_id_on_message(&mut source, &provisional);
        set_source(&mut submission, *source.clone());
    } else {
        copies(&mut submission, &source);
    }
    let retry = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("retry");
    assert_eq!(retry.message_key, first.message_key);
    detached::execute(&fixture, &retry, &connections, &sm).await;
    if old_row || occupant_case == OccupantCase::Missing {
        if occupant_case == OccupantCase::Missing {
            assert!(
                detached::queued(&sm, &a).await.unacked_stanzas.is_empty(),
                "no first non-sender copy reconstructed without occupant-id"
            );
        }
        assert!(
            detached::queued(&sm, &b).await.unacked_stanzas.is_empty(),
            "no non-sender reconstruction without provenance"
        );
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NULL")
                .await,
            1
        );
    } else {
        let rebuilt = wire(&sm, &b).await;
        assert_wire(&rebuilt, &b, system);
        if occupant_case == OccupantCase::Present {
            assert_same_occupant(original.as_ref().expect("original copy"), &rebuilt);
        }
        assert_eq!(detached::queued(&sm, &a).await.unacked_stanzas.len(), 1);
        if system {
            assert_eq!(
                detached::queued(&sm, &submission.sender)
                    .await
                    .unacked_stanzas
                    .len(),
                1,
                "system initiator is a completed occupant, not a retry reflection"
            );
        }
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            1
        );
    }
    if reconnect || old_row {
        let reflection = wire(&sm, &submission.sender).await;
        assert_eq!(reflection.to, Some(submission.sender.clone().into()));
        assert_eq!(
            reflection.from.as_ref().map(jid::Jid::to_bare),
            Some(room())
        );
        if old_row {
            assert_eq!(reflection.from, source.from);
        }
    }
    drop(sm);
    fixture.close().await;
}

struct Environment {
    connections: ConnectionRegistry,
    sm: Arc<InMemorySmSessionRegistry>,
}
impl RecoveryEnvironment for Environment {
    fn recovery_deps(&self) -> waddle_server::ingress::Deps<'_> {
        let mut deps = waddle_server::ingress::Deps::new(&self.connections, "example.com");
        deps.sm_session_registry = Some(&self.sm);
        deps
    }
}

pub async fn maintenance(fixture: IngressFixture) {
    maintenance_case(fixture, false).await;
}

pub async fn occupant_maintenance(fixture: IngressFixture) {
    maintenance_case(fixture, true).await;
}

async fn maintenance_case(fixture: IngressFixture, compare_occupant: bool) {
    let sm = detached::registry(&fixture).await;
    let [a, b, _] = detached::resources();
    let submission = make_submission(&fixture, false);
    detached::attach(&sm, &a).await;
    detached::attach(&sm, &submission.sender).await;
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("accept");
    detached::execute(&fixture, &first, &ConnectionRegistry::new(), &sm).await;
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 1);
    let original = wire(&sm, &a).await;
    detached::attach(&sm, &b).await;
    let authority = fixture.authority().await;
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(Environment {
        connections: ConnectionRegistry::new(),
        sm: sm.clone(),
    });
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL",
        waddle_server::db::DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL",
    };
    fixture
        .execute(
            sql,
            waddle_server::db_params![
                (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339()
            ],
        )
        .await;
    authority.trigger_maintenance();
    tokio::time::timeout(Duration::from_secs(15), async {
        while fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await
            != 1
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maintenance settled pending occupant");
    let rebuilt = wire(&sm, &b).await;
    assert_wire(&rebuilt, &b, false);
    if compare_occupant {
        assert_same_occupant(&original, &rebuilt);
    }
    assert_eq!(detached::queued(&sm, &a).await.unacked_stanzas.len(), 1);
    assert_eq!(
        detached::queued(&sm, &submission.sender)
            .await
            .unacked_stanzas
            .len(),
        1,
        "maintenance excludes reflection"
    );
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(environment);
    drop(authority);
    drop(sm);
    fixture.close().await;
}
