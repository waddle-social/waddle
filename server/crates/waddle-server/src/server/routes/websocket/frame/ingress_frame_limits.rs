use super::*;
use crate::server::routes::websocket::tests::{
    create_test_session, create_test_websocket_state, create_test_websocket_state_with_db_pool,
};
use std::sync::Arc;
use waddle_xmpp::pending_delivery::SmSessionId;

async fn connection(state: &WebSocketState, resumable: bool) -> WsConnState {
    let mut conn = WsConnState::new();
    let jid = "alice@example.com/web"
        .parse::<jid::FullJid>()
        .expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(state, "alice").await);
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        jid,
        false,
        Default::default(),
    );
    if resumable {
        let id = SmSessionId::new(uuid::Uuid::new_v4().to_string());
        drop(
            state
                .deps
                .protocol
                .sm_session_registry
                .ensure_session_claim(id.as_str())
                .await
                .expect("claim"),
        );
        state
            .deps
            .protocol
            .ingress
            .enroll_stream(&id)
            .await
            .expect("enroll");
        conn.sm_ingress_fence = state
            .deps
            .protocol
            .sm_session_registry
            .current_sm_claim_fence(id.as_str());
        conn.sm_state
            .enable(id.as_str().to_owned(), true, Some(300));
    }
    conn
}

async fn sql(state: &WebSocketState, query: &str) {
    state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db")
        .execute(query, ())
        .await
        .expect("execute");
}

async fn count(state: &WebSocketState, query: &str) -> i64 {
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db");
    let mut rows = db.query(query, ()).await.expect("query");
    rows.next()
        .await
        .expect("row")
        .expect("count")
        .get(0)
        .expect("integer")
}

fn limit_message(case: usize) -> xmpp_parsers::message::Message {
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("target")));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "over-limit");
    match case {
        0 => {
            message
                .bodies
                .insert(Default::default(), "x".repeat(65_537));
        }
        1 => {
            let ns = waddle_xmpp::xep::xep0334::NS_HINTS;
            let mut element = Element::builder("store", ns).build();
            for _ in 0..16 {
                element = Element::builder("store", ns).append(element).build();
            }
            message.payloads.push(element);
        }
        _ => {
            for language in ["en", "fr", "de", "es", "no"] {
                message.bodies.insert(
                    xmpp_parsers::message::Lang(language.to_owned()),
                    "x".repeat(60_000),
                );
            }
        }
    }
    message
}

async fn resource_limit_case(state: Arc<WebSocketState>, resumable: bool) {
    let mut conn = connection(&state, resumable).await;
    for case in 0..3 {
        let message = limit_message(case);
        let wire = super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
        let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
        let reply = frames
            .iter()
            .find_map(|frame| {
                frame
                    .parse::<Element>()
                    .ok()
                    .filter(|element| element.name() == "message")
            })
            .expect("stanza error");
        let error = xmpp_parsers::stanza_error::StanzaError::try_from(
            reply
                .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
                .expect("error")
                .clone(),
        )
        .expect("typed stanza error");
        assert_eq!(
            error.defined_condition,
            xmpp_parsers::stanza_error::DefinedCondition::ResourceConstraint
        );
        assert!(!conn.sm_inbound_completion.has_unhandled_hole());
        assert!(conn.phase.is_ready());
        if resumable {
            assert_eq!(conn.sm_state.get_inbound_count(), case as u32 + 1);
            let id = SmSessionId::new(conn.sm_state.stream_id.as_deref().expect("stream"));
            assert_eq!(
                state
                    .deps
                    .protocol
                    .ingress
                    .load_resume_checkpoint(&id)
                    .await
                    .expect("checkpoint")
                    .expect("stream")
                    .to_storage(),
                case as u32 + 1
            );
        }
    }
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ingress_messages").await,
        3
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ingress_origin_aliases").await,
        0
    );
}

async fn stream_lookup_failure_case(state: Arc<WebSocketState>) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let mut conn = connection(&state, true).await;
    let stream = SmSessionId::new(conn.sm_state.stream_id.as_deref().expect("stream"));
    sql(
        &state,
        "ALTER TABLE ingress_sm_streams RENAME TO ingress_sm_streams_unavailable",
    )
    .await;
    let wire = super::super::transport_xml::stanza_to_xml(&Stanza::Message(limit_message(0)));
    let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
    assert!(frames.is_empty());
    assert!(conn.sm_inbound_completion.has_unhandled_hole());
    assert_eq!(conn.sm_state.get_inbound_count(), 0);
    assert_eq!(
        metrics.counter_sum("ingress.decisions", &[("class", "storage")]),
        Some(1)
    );
    assert_eq!(metrics.counter_sum("ingress.decisions", &[]), Some(1));
    sql(
        &state,
        "ALTER TABLE ingress_sm_streams_unavailable RENAME TO ingress_sm_streams",
    )
    .await;
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&stream)
            .await
            .expect("checkpoint")
            .expect("stream")
            .to_storage(),
        0
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ingress_messages").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ingress_origin_aliases").await,
        0
    );
    let mut healthy = connection(&state, false).await;
    healthy
        .sm_state
        .enable(stream.as_str().to_owned(), true, Some(300));
    healthy.sm_ingress_fence = conn.sm_ingress_fence.clone();
    let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut healthy).await;
    assert!(frames
        .iter()
        .any(|frame| frame.contains("resource-constraint")));
    assert_eq!(healthy.sm_state.get_inbound_count(), 1);
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&stream)
            .await
            .expect("healthy checkpoint")
            .expect("stream")
            .to_storage(),
        1
    );
}

async fn postgres_state() -> Option<(Arc<WebSocketState>, sqlx::PgPool, String)> {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        return None;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres");
    let schema = format!("ingress_frame_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("schema");
    let mut url = url::Url::parse(&database_url).expect("URL");
    url.query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema}"));
    let pool = Arc::new(
        crate::db::DatabasePool::new(
            crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url.to_string()),
            crate::db::PoolConfig,
        )
        .await
        .expect("pool"),
    );
    Some((
        create_test_websocket_state_with_db_pool(pool).await,
        admin,
        schema,
    ))
}

async fn cleanup(admin: sqlx::PgPool, schema: String) {
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
    admin.close().await;
}

#[tokio::test]
async fn ingress_digest_limits_advance_resumable_sqlite() {
    resource_limit_case(create_test_websocket_state().await, true).await;
}
#[tokio::test]
async fn ingress_digest_limits_reject_ephemeral_sqlite() {
    resource_limit_case(create_test_websocket_state().await, false).await;
}
#[tokio::test]
async fn ingress_digest_limits_advance_resumable_postgres() {
    let Some((state, admin, schema)) = postgres_state().await else {
        return;
    };
    resource_limit_case(state, true).await;
    cleanup(admin, schema).await;
}
#[tokio::test]
async fn ingress_digest_limits_reject_ephemeral_postgres() {
    let Some((state, admin, schema)) = postgres_state().await else {
        return;
    };
    resource_limit_case(state, false).await;
    cleanup(admin, schema).await;
}
#[tokio::test]
async fn ingress_stream_lookup_failure_meters_storage_once_sqlite() {
    stream_lookup_failure_case(create_test_websocket_state().await).await;
}
#[tokio::test]
async fn ingress_stream_lookup_failure_meters_storage_once_postgres() {
    let Some((state, admin, schema)) = postgres_state().await else {
        return;
    };
    stream_lookup_failure_case(state).await;
    cleanup(admin, schema).await;
}

#[test]
fn ingress_resource_rejection_digest_preserves_offered_identity_without_alias() {
    let context = waddle_xmpp::ingress::DigestContext {
        target: waddle_xmpp::ingress::NormalizedTarget::Bare(
            "bob@example.com".parse().expect("target"),
        ),
        server_authorities: vec![],
        stanza_lang: None,
    };
    let first = limit_message(0);
    let mut changed = first.clone();
    changed
        .bodies
        .get_mut(&xmpp_parsers::message::Lang::default())
        .expect("body")
        .push('y');
    let digest = resource_rejection_digest(&first, &context).expect("resource digest");
    let repeated = resource_rejection_digest(&first, &context).expect("repeat digest");
    let different = resource_rejection_digest(&changed, &context).expect("different digest");
    assert!(digest.origin().is_none());
    assert_eq!(
        waddle_xmpp::ingress::digest::v1::digest(&digest),
        waddle_xmpp::ingress::digest::v1::digest(&repeated)
    );
    assert_ne!(
        waddle_xmpp::ingress::digest::v1::digest(&digest),
        waddle_xmpp::ingress::digest::v1::digest(&different)
    );
}
