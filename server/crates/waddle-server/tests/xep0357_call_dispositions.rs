//! Call expiry and node applicability settle canonical custody without a send.
#![cfg(feature = "test-support")]

pub mod ingress_support;

use std::sync::Arc;

use ingress_support::IngressFixture;
use jid::BareJid;
use minidom::Element;
use p256::pkcs8::EncodePrivateKey;
use uuid::Uuid;
use waddle_server::ingress_uow::{
    CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
};
use waddle_server::notification_outbox::WADDLE_PUSH_CONTEXT_NS;
use waddle_server::push_service::DatabasePushServiceStore;
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};
use waddle_xmpp::push::apns::{
    ApnsKeyId, ApnsOutcome, ApnsRequest, ApnsSender, ApnsTeamId, ApnsTopic, CachingApnsTokenSigner,
    SystemApnsClock,
};
use waddle_xmpp::xep::xep0357::NS_PUSH;

const TOPIC: &str = "social.waddle.tests";

struct NeverSend;
impl ApnsSender for NeverSend {
    fn send(
        &self,
        _request: ApnsRequest<'_>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ApnsOutcome> + Send + '_>> {
        Box::pin(async { panic!("terminal call policies must never contact a provider") })
    }
}

fn with_apns(store: DatabasePushServiceStore) -> DatabasePushServiceStore {
    let pem = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng)
        .to_pkcs8_pem(Default::default())
        .expect("test key");
    let tokens = CachingApnsTokenSigner::from_pkcs8_pem(
        ApnsTeamId::parse("TEAM123456").expect("team"),
        ApnsKeyId::parse("KEY1234567").expect("key id"),
        &pem,
        Arc::new(SystemApnsClock),
    )
    .expect("signer");
    store
        .with_apns_provider(
            Arc::new(tokens),
            Arc::new(NeverSend),
            ApnsTopic::parse(TOPIC).expect("topic"),
        )
        .expect("VoIP topic")
}

fn notification(expired: bool, call: bool) -> Element {
    let mut context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            "bob@example.com",
        )
        .attr(
            minidom::rxml::xml_ncname!("class").to_owned(),
            if call { "call" } else { "dm" },
        );
    if call {
        context = context.append(
            Element::builder("call", WADDLE_PUSH_CONTEXT_NS)
                .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "session")
                .attr(minidom::rxml::xml_ncname!("video").to_owned(), "false")
                .attr(
                    minidom::rxml::xml_ncname!("expires").to_owned(),
                    if expired { "1" } else { "4102444800" },
                )
                .build(),
        );
    }
    Element::builder("notification", NS_PUSH)
        .append(context.build())
        .build()
}

async fn attach_custody(fixture: &IngressFixture, owner: &BareJid, delivery: Uuid) {
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: owner.clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: "bob@example.com".parse().expect("conversation"),
            archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                delivery.to_string(),
                owner.clone().into(),
            ),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("canonical parent");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [63; 32]).expect("digest"),
        None,
    )
    .await
    .expect("parent");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("intent");
    EffectDescendantRepository::attach(&mut tx, key, &intent.semantic_key(), delivery)
        .await
        .expect("custody");
    tx.commit().await.expect("commit custody");
}

async fn authoritative_dispositions(fixture: IngressFixture) {
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let base = DatabasePushServiceStore::new_with_secret_key(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
    )
    .await
    .expect("push service");
    let configured = with_apns(base.clone());
    let topic_only = base
        .clone()
        .with_apns_topic(ApnsTopic::parse(TOPIC).expect("topic"))
        .expect("topic without provider");
    for (expired, call, provider, known_topic, voip_node) in [
        (false, true, true, true, false),
        (true, true, true, true, true),
        (true, true, true, true, false),
        (false, false, true, true, true),
        (false, true, false, false, false),
        (false, true, false, true, false),
        (false, false, false, true, true),
    ] {
        let expect_expired = expired && voip_node;
        for uncertain in [false, true] {
            let push = if provider {
                &configured
            } else if known_topic {
                &topic_only
            } else {
                &base
            };
            let app_id = if voip_node {
                format!("{TOPIC}.voip")
            } else {
                format!("{TOPIC}.other-{provider}")
            };
            let node = push.ensure_node(&owner, &app_id).await.expect("node");
            let job = Uuid::new_v4();
            let delivery = Uuid::new_v4();
            let payload = notification(expired, call);
            // Persist an accepted canonical job with no active devices. A
            // device-empty retry must not precede applicability or expiry.
            fixture.execute(
                "INSERT INTO push_publish_jobs (job_id, owner_bare_jid, node, item_id, payload_xml, ancestry_job_id, acceptance_scope, backing_state, status, uncertain_send, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, 'canonical', 'published', 'queued', ?, 1, 1)",
                waddle_server::db_params![job.to_string(), owner.to_string(), node.node(), delivery.to_string(), String::from(&payload), delivery.to_string(), i64::from(uncertain)],
            ).await;
            attach_custody(&fixture, &owner, delivery).await;
            let outcomes = push
                .drain_queued_notification_publish_jobs(16)
                .await
                .expect("policy disposition");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].attempted_devices(), 0);
            let mut rows = fixture.db.guard().await.expect("database").query(
                "SELECT status, uncertain_send, last_error, next_retry_at_ms, claim_token FROM push_publish_jobs WHERE job_id = ?",
                waddle_server::db_params![job.to_string()],
            ).await.expect("job state");
            let row = rows.next().await.expect("row").expect("job");
            assert_eq!(
                row.get::<String>(0).expect("status"),
                if expect_expired {
                    "failed"
                } else {
                    "published"
                }
            );
            assert_eq!(row.get::<i64>(1).expect("uncertainty"), 0);
            assert!(row
                .get::<String>(2)
                .expect("diagnostic")
                .contains(if expect_expired {
                    "call expired"
                } else {
                    "not-applicable"
                }));
            assert_eq!(row.get::<Option<i64>>(3).expect("retry"), None);
            assert_eq!(row.get::<Option<String>>(4).expect("claim"), None);
            drop(rows);
            assert!(push
                .delivery_attempts_for_node(node.node())
                .await
                .expect("attempts")
                .is_empty());
            let mut rows = fixture.db.guard().await.expect("database").query(
                "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL",
                waddle_server::db_params![delivery.to_string()],
            ).await.expect("custody");
            assert_eq!(
                rows.next()
                    .await
                    .expect("row")
                    .expect("count")
                    .get::<i64>(0)
                    .expect("unsettled count"),
                0
            );
        }
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_call_policies_clear_uncertainty_and_settle_ancestry() {
    authoritative_dispositions(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_call_policies_clear_uncertainty_and_settle_ancestry() {
    if let Some(fixture) = IngressFixture::postgres("pushcallpolicies").await {
        authoritative_dispositions(fixture).await;
    }
}
