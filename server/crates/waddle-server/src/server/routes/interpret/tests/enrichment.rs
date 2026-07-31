use super::*;

#[tokio::test]
async fn enrichment_request_without_extension_manager_fails_open_with_original_message() {
    // No extension manager in Deps -> the original typed message
    // is returned unchanged via EnrichmentComplete. This is the
    // legacy fail-open contract (see `enrich_message` in the
    // legacy `message.rs` path).
    use waddle_xmpp::protocol::event::CallbackId;
    let guard = waddle_xmpp::telemetry::test_support::acquire().await;
    let registry = ConnectionRegistry::new();
    let deps = Deps::registry_only(&registry);

    let mut original = chat_msg("alice@example.com/web", "bob@example.com", "look https://x");
    original.id = Some(xmpp_parsers::message::Id("orig-id".to_string()));

    let events = vec![OutboundEvent::RequestEnrichment {
        id: CallbackId(42),
        message: Box::new(original.clone()),
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::EnrichmentComplete {
            id: CallbackId(42),
            message,
        } => {
            assert_eq!(message.id, original.id);
            assert_eq!(
                message.bodies.get("").cloned(),
                Some("look https://x".to_string()),
            );
        }
        other => panic!("expected EnrichmentComplete, got {other:?}"),
    }

    // #1320: every enrichment pass times itself, even the fail-open
    // no-op path that adds zero embeds.
    assert_eq!(
        guard.histogram_count("xmpp.extensions.enrichment.latency", &[]),
        Some(1),
        "enrichment must record one latency sample per pass",
    );
}

#[tokio::test]
async fn enrichment_failure_fail_open_feeds_original_message_back() {
    // Fail-open contract: when the extension manager has no
    // working actors (e.g. all extension RPCs failed at startup,
    // or the deployment intentionally disabled extensions),
    // `enrich_message` is a no-op and the dispatch must still
    // resume with the *original* message via EnrichmentComplete
    // — never block on enrichment, never drop the message.
    // We model this with a disabled config (no actors loaded),
    // which is the exact failure mode legacy `message.rs` falls
    // back to when the wasm runtime can't start any extension.
    use waddle_extensions::{ExtensionConfig, ExtensionManager};
    use waddle_xmpp::protocol::event::CallbackId;

    let registry = ConnectionRegistry::new();
    let em = Arc::new(
        ExtensionManager::from_config(ExtensionConfig {
            enabled: false,
            ..Default::default()
        })
        .await
        .expect("disabled extension manager"),
    );
    let deps = Deps::test_with_extension_manager(&registry, &em);

    let mut original = chat_msg(
        "alice@example.com/web",
        "bob@example.com",
        "check https://example.com",
    );
    original.id = Some(xmpp_parsers::message::Id("fail-open-id".to_string()));
    let original_payload_count = original.payloads.len();

    let events = vec![OutboundEvent::RequestEnrichment {
        id: CallbackId(123),
        message: Box::new(original.clone()),
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::EnrichmentComplete {
            id: CallbackId(123),
            message,
        } => {
            assert_eq!(
                message.id.as_ref().map(|id| id.0.as_str()),
                Some("fail-open-id"),
                "fail-open path returns the original message id"
            );
            assert_eq!(
                message.bodies.get("").cloned(),
                original.bodies.get("").cloned(),
                "fail-open path returns the original body unchanged"
            );
            assert_eq!(
                message.payloads.len(),
                original_payload_count,
                "fail-open path adds no payloads when no actor produces enrichment"
            );
        }
        other => panic!("expected EnrichmentComplete, got {other:?}"),
    }
}

#[tokio::test]
async fn enrichment_request_calls_extension_manager_and_feeds_complete_back() {
    // Wire a real (empty) ExtensionManager — no extension actors
    // configured, so `enrich_message` returns 0 enrichments and
    // we still feed back the original message via
    // EnrichmentComplete with the original CallbackId. This proves
    // the callback round-trip without depending on a live wasm
    // runtime.
    use waddle_extensions::{ExtensionConfig, ExtensionManager};
    use waddle_xmpp::protocol::event::CallbackId;

    let registry = ConnectionRegistry::new();
    let em = Arc::new(
        ExtensionManager::from_config(ExtensionConfig {
            enabled: false,
            ..Default::default()
        })
        .await
        .expect("disabled extension manager"),
    );
    let deps = Deps::test_with_extension_manager(&registry, &em);

    let mut original = chat_msg("alice@example.com/web", "bob@example.com", "ping");
    original.id = Some(xmpp_parsers::message::Id("e-id".to_string()));

    let events = vec![OutboundEvent::RequestEnrichment {
        id: CallbackId(99),
        message: Box::new(original),
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::EnrichmentComplete {
            id: CallbackId(99),
            message,
        } => {
            assert_eq!(message.id.as_ref().map(|id| id.0.as_str()), Some("e-id"));
        }
        other => panic!("expected EnrichmentComplete, got {other:?}"),
    }
}

// -----------------------------------------------------------------
