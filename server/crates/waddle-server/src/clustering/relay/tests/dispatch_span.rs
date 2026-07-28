use super::*;
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType};

/// #1485: when the sending node propagated its W3C trace context, the
/// receiving node's dispatch root must join that trace instead of
/// starting its own — the whole point of the propagation.
#[test]
fn relay_dispatch_span_joins_a_propagated_sender_trace() {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();

    // Sending node: an active span whose context is stamped onto the
    // relay message at the send seam.
    let sender = tracing::info_span!("clustering.relay.send-under-test");
    let (trace, sender_trace_id, sender_span_id) = sender.in_scope(|| {
        let span_context = tracing::Span::current()
            .context()
            .span()
            .span_context()
            .clone();
        (
            RelayTraceContext::capture(),
            span_context.trace_id(),
            span_context.span_id(),
        )
    });
    assert_ne!(
        trace,
        RelayTraceContext::default(),
        "an active, valid sender span must yield a propagatable context"
    );

    // Receiving node: the same context after a real codec round-trip.
    let encoded = rmp_serde::to_vec_named(&trace).expect("trace context encodes");
    let decoded: RelayTraceContext =
        rmp_serde::from_slice(&encoded).expect("trace context decodes");
    let dispatch = relay_dispatch_span("cross_node_check", &decoded);
    drop(dispatch);
    drop(sender);

    let exported = spans.exported();
    let dispatch = exported
        .iter()
        .find(|span| span.name == "clustering.relay.dispatch")
        .expect("dispatch span must export");
    assert_eq!(
        dispatch.span_context.trace_id(),
        sender_trace_id,
        "the receiving node's dispatch span must join the sender's trace"
    );
    assert_eq!(
        dispatch.parent_span_id, sender_span_id,
        "the dispatch span must be parented on the sending span"
    );
}
/// #1485 mixed-version rolling deploy, old sender → new receiver: a
/// relay message encoded WITHOUT the additive trace field must still
/// decode, with an empty context that falls back to a root dispatch
/// span.
#[test]
fn a_relay_message_without_the_trace_field_still_decodes() {
    /// The pre-#1485 wire shape of [`Demote`].
    #[derive(Serialize, Deserialize)]
    struct LegacyDemote {
        entity: Entity,
        new_epoch: ClaimEpoch,
    }

    let entity = Entity::new(EntityType::RoomActor, "room@muc.example.com".to_string());
    let encoded = rmp_serde::to_vec_named(&LegacyDemote {
        entity: entity.clone(),
        new_epoch: ClaimEpoch(11),
    })
    .expect("legacy demote encodes");

    let decoded: Demote = rmp_serde::from_slice(&encoded).expect("legacy demote decodes");
    assert_eq!(decoded.entity, entity);
    assert_eq!(decoded.new_epoch, ClaimEpoch(11));
    assert_eq!(
        decoded.trace,
        RelayTraceContext::default(),
        "an absent trace field must default to no context, not fail the decode"
    );
}
/// #1485 mixed-version rolling deploy, new sender → old receiver: the
/// pre-#1485 decoder must ignore the extra field rather than reject the
/// message (serde's derived `Deserialize` skips unknown map keys, and
/// kameo encodes remote messages as named maps).
#[test]
fn an_older_decoder_ignores_the_added_trace_field() {
    #[derive(Serialize, Deserialize)]
    struct LegacyDemote {
        entity: Entity,
        new_epoch: ClaimEpoch,
    }

    let entity = Entity::new(EntityType::RoomActor, "room@muc.example.com".to_string());
    let encoded = rmp_serde::to_vec_named(&Demote {
        entity: entity.clone(),
        new_epoch: ClaimEpoch(13),
        trace: RelayTraceContext::default(),
    })
    .expect("demote encodes");

    let legacy: LegacyDemote =
        rmp_serde::from_slice(&encoded).expect("pre-#1485 decoder tolerates the new field");
    assert_eq!(legacy.entity, entity);
    assert_eq!(legacy.new_epoch, ClaimEpoch(13));
}
/// #1483 guard: every delegated relay reply must be spawned through
/// `spawn_in_dispatch_span`, the one seam that binds the reply task
/// to its dispatch span. A direct `ctx.spawn` in a handler would run
/// the delivery — where the actor messages happen — outside the root
/// span, silently restoring the #1438 trace loss, and the
/// field-recording tests above cannot catch that (the span still
/// records its fields at creation). Comment lines are skipped; no
/// parsing beyond that is needed, so string/paren contents cannot
/// cause false failures.
#[test]
fn delegated_relay_replies_go_through_the_dispatch_span_helper() {
    let source = include_str!("../../relay.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("split always yields a first segment");
    let direct_spawns = production
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| line.contains("ctx.spawn("))
        .count();
    assert_eq!(
        direct_spawns, 1,
        "ctx.spawn must appear exactly once — inside spawn_in_dispatch_span; \
         route new delegated replies through that helper"
    );
}

/// #1594: a re-assert ask against a relay whose delivery bridge has
/// no wired services (this node's `WebSocketState` is unreachable)
/// must answer `Unavailable` — never a fabricated occupancy answer,
/// and never a hang.
#[tokio::test(flavor = "current_thread")]
async fn reassert_media_grants_without_wired_services_answers_unavailable() {
    // Thread-scoped subscriber, not asserted on: a relay ask on a
    // subscriber-less thread destabilizes the interest cache the
    // *_records_the_dispatch_span tests depend on when the tests
    // overlap (pre-existing test-support limitation, observed
    // deterministically pairwise).
    let _spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let actor_ref = spawn_test_relay_actor();

    let reply = actor_ref
        .ask(RelayReassertMediaGrants {
            room: "room@muc.example.com".parse().expect("room jid"),
            participant: "alice@example.com/web".parse().expect("participant jid"),
            trace: RelayTraceContext::default(),
        })
        .await
        .expect("reassert ask succeeds");

    assert_eq!(reply, RelayReassertMediaGrantsReply::Unavailable);
}

/// #1594: the re-assert handler delegates its reply (the owner-side
/// room-actor ask is bounded but slow-able), and delegated work must
/// still run under the named relay dispatch root span (#1483).
#[tokio::test(flavor = "current_thread")]
async fn reassert_media_grants_ask_records_the_dispatch_span() {
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let actor_ref = spawn_test_relay_actor();

    let _reply = actor_ref
        .ask(RelayReassertMediaGrants {
            room: "room@muc.example.com".parse().expect("room jid"),
            participant: "alice@example.com/web".parse().expect("participant jid"),
            trace: RelayTraceContext::default(),
        })
        .await
        .expect("reassert ask succeeds");

    assert_eq!(
        spans
            .recorded_field("clustering.relay.dispatch", "relay.message")
            .as_deref(),
        Some("reassert_media_grants"),
        "reassert handling must run under the named relay dispatch root span"
    );
}

/// #1483: an inbound relay ask handled inline (no delegated reply) must
/// open the named `clustering.relay.dispatch` root span, so the actor
/// work it triggers is parented and survives the #1438 span-noise
/// sampler.
#[tokio::test(flavor = "current_thread")]
async fn inline_relay_ask_records_the_dispatch_span() {
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let actor_ref = spawn_test_relay_actor();

    let reply = actor_ref
        .ask(Demote {
            entity: Entity::new(EntityType::RoomActor, "room@muc.example.com".to_string()),
            new_epoch: ClaimEpoch(7),
            trace: RelayTraceContext::default(),
        })
        .await
        .expect("demote ask succeeds");
    assert_eq!(reply, DemoteReply::Acked);

    assert_eq!(
        spans
            .recorded_field("clustering.relay.dispatch", "relay.message")
            .as_deref(),
        Some("demote"),
        "demote handling must run under the named relay dispatch root span"
    );
}

/// #1483: a delegated-reply relay ask must carry the named dispatch span
/// onto the spawned reply task, so the whole delivery — not just the
/// mailbox slice — is covered by the root span.
#[tokio::test(flavor = "current_thread")]
async fn delegated_relay_ask_records_the_dispatch_span() {
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let actor_ref = spawn_test_relay_actor();

    // No live local connection for the stream: the delegated task
    // resolves quickly with NotLiveLocally.
    let reply = actor_ref
        .ask(RelayResumeSteal {
            stream_id: waddle_xmpp::pending_delivery::SmSessionId::new("span-test-stream"),
            requester_bare_jid: "alice@example.com".parse().expect("valid bare jid"),
            trace: RelayTraceContext::default(),
        })
        .await
        .expect("resume-steal ask succeeds");
    assert_eq!(reply, RelayResumeStealReply::NotLiveLocally);

    assert_eq!(
        spans
            .recorded_field("clustering.relay.dispatch", "relay.message")
            .as_deref(),
        Some("resume_steal"),
        "resume-steal handling must run under the named relay dispatch root span"
    );
    assert_eq!(
        spans
            .recorded_field("clustering.relay.dispatch", "stream_id")
            .as_deref(),
        Some("span-test-stream"),
        "the dispatch span must carry the stream id"
    );
}

/// #1483: `parent: None` is the load-bearing property — the handlers
/// run inside kameo's own suppressed root `actor.handle_message` span,
/// and a child of a locally-unsampled parent is dropped by the #1438
/// sampler too. Pin that the production constructor starts a fresh
/// root even when a span is active.
#[tokio::test(flavor = "current_thread")]
async fn relay_dispatch_span_is_a_root_even_inside_an_active_span() {
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let outer = tracing::info_span!("actor.handle_message");
    let dispatch =
        outer.in_scope(|| relay_dispatch_span("root_check", &RelayTraceContext::default()));
    drop(dispatch);
    drop(outer);

    let exported = spans.exported();
    let dispatch = exported
        .iter()
        .find(|span| span.name == "clustering.relay.dispatch")
        .expect("dispatch span must export");
    assert_eq!(
        dispatch.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "the dispatch span must root a fresh trace, not inherit the \
         active (suppressed) actor span as its parent"
    );
}
