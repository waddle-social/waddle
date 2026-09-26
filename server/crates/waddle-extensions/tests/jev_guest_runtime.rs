//! Exercises the built Jev component across separate init and invocation stores.
//! The waddle-server-extension-runtime task supplies WADDLE_JEV_GUEST_WASM after building it.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use waddle_extensions::host_tools::InvocationKind;
use waddle_extensions::runtime::{LoadedExtension, WasmRuntime};
use waddle_extensions::{
    DenyingExtensionHostTools, DisplayText, ExtensionCapability, ExtensionEvent, InvocationContext,
    MessageRevision, ObservationFailure, OriginId, PluginId, RoomMessageObserve, RoomMessageSource,
    Sha256Digest, StanzaId, Timestamp, WaddleId,
};

fn observation() -> ExtensionEvent {
    ExtensionEvent::RoomMessageObserve(RoomMessageObserve {
        source: RoomMessageSource {
            room: "room@muc.example.com".parse().expect("room"),
            stanza_id: StanzaId::new("room-stanza").expect("stanza"),
            revision_stanza_id: StanzaId::new("room-stanza").expect("revision"),
            origin_id: Some(OriginId::new("source-origin").expect("origin")),
            sender: "alice@example.com".parse().expect("sender"),
            revision: MessageRevision::new(0),
            body_digest: Sha256Digest::new(
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            )
            .expect("digest"),
            observed_at: Timestamp::new("2026-09-26T12:00:00Z").expect("timestamp"),
        },
        body: DisplayText::new("hello").expect("body"),
    })
}

fn context() -> InvocationContext {
    InvocationContext {
        waddle_id: WaddleId::new("local").expect("waddle"),
        plugin_id: PluginId::new("jev-judgments").expect("plugin"),
        requester: None,
        source_room: Some("room@muc.example.com".parse().expect("room")),
        kind: InvocationKind::RoomMessageObserve,
        provider_room_grants: Vec::new(),
    }
}

#[tokio::test]
async fn jev_fresh_instance_reads_its_config_before_http() {
    let Some(path) = std::env::var_os("WADDLE_JEV_GUEST_WASM") else {
        // Plain cargo test does not build guest WASM. The extension build gate
        // supplies this path and executes this test against the actual artifact.
        return;
    };
    let runtime = WasmRuntime::new().expect("runtime");
    let extension = LoadedExtension::load(&runtime, Path::new(&path)).expect("guest component");
    let valid_config = r#"{"api_key":"fake-test-key"}"#;
    extension.call_init(valid_config).await.expect("guest init");
    let grants = HashSet::from([ExtensionCapability::OutboundHttpRequest]);

    // No allowed origins: the host rejects the request before any network I/O.
    // TemporaryFailure proves handle_event parsed the config in its own fresh
    // instance and reached the HTTP import rather than failing uninitialized.
    let valid = extension
        .call_handle_event_typed(
            observation(),
            Arc::new(DenyingExtensionHostTools),
            context(),
            valid_config.into(),
            grants.clone(),
            Vec::new(),
        )
        .await;
    assert!(matches!(valid, Err(ObservationFailure::TemporaryFailure)));

    let invalid = extension
        .call_handle_event_typed(
            observation(),
            Arc::new(DenyingExtensionHostTools),
            context(),
            r#"{"api_key":""}"#.into(),
            grants,
            Vec::new(),
        )
        .await;
    assert!(matches!(invalid, Err(ObservationFailure::InvalidRequest)));
}
