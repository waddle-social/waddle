//! T1-F09-04: Wasm fuel and wall-clock epoch interruption terminate runaway guests.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::tempdir;
use waddle_broker_core::identity::TenantId;
use waddle_extensions::config::RuntimeLimits;
use waddle_extensions::host_tools::{DenyingExtensionHostTools, InvocationContext, InvocationKind};
use waddle_extensions::runtime::{LoadedExtension, WasmRuntime};
use waddle_extensions::types::{
    CommandAction, CommandInvocation, CommandNode, ExtensionEvent, FullJidValue,
};
use waddle_extensions::{PluginId, WaddleId};

const BASE_COMPONENT_WAT: &str =
    include_str!("../../waddle-extensions/tests/fixtures/message_hook.wat");

fn spinning_wat() -> String {
    let needle = "      i32.const 1024\n    )";
    assert!(
        BASE_COMPONENT_WAT.contains(needle),
        "expected handle-event return sequence in base fixture"
    );
    BASE_COMPONENT_WAT.replacen(
        needle,
        "      (loop $spin (br $spin))\n      i32.const 1024\n    )",
        1,
    )
}

fn command_event_and_context(tenant: &TenantId) -> (ExtensionEvent, InvocationContext) {
    let event = ExtensionEvent::Command(CommandInvocation {
        waddle_id: WaddleId::try_new(tenant.as_str()).expect("waddle id"),
        room: None,
        requester: FullJidValue::try_new("alice@acme.example/web").expect("requester"),
        command_node: CommandNode::try_new("urn:waddle:extension:1:spin").expect("node"),
        session_id: None,
        action: Some(CommandAction::Execute),
        form: None,
        fields: Vec::new(),
    });
    let ctx = InvocationContext {
        tenant_id: Some(tenant.clone()),
        waddle_id: WaddleId::try_new(tenant.as_str()).expect("waddle id"),
        plugin_id: PluginId::try_new("spin-guest").expect("plugin id"),
        requester: Some("alice@acme.example".parse().expect("bare jid")),
        source_room: None,
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    (event, ctx)
}

#[tokio::test]
async fn wasm_fuel_and_epoch_interruption_terminate_runaway_guests() {
    let dir = tempdir().expect("tempdir");
    let wat_path = dir.path().join("spin.wat");
    std::fs::write(&wat_path, spinning_wat()).expect("write spin.wat");

    let runtime = WasmRuntime::try_new().expect("create WasmRuntime");
    let tenant = TenantId::parse("acme.example").expect("valid tenant");
    let tools = Arc::new(DenyingExtensionHostTools);

    // 1. Fuel budget traps a tight infinite loop well before timeout.
    let fuel_ext = LoadedExtension::load(&runtime, &wat_path)
        .expect("load spin component")
        .with_limits(RuntimeLimits {
            wasm_fuel: 25_000,
            invocation_timeout_ms: 5_000,
            http_timeout_ms: 5_000,
            ..RuntimeLimits::default()
        })
        .expect("valid fuel limits");
    let (event, ctx) = command_event_and_context(&tenant);
    let fuel_start = Instant::now();
    let fuel_err = fuel_ext
        .call_handle_event(
            event,
            tools.clone(),
            ctx,
            "{}".to_owned(),
            HashSet::new(),
            Vec::new(),
        )
        .await
        .expect_err("spin guest must trap on fuel exhaustion");
    assert!(
        fuel_start.elapsed() < Duration::from_secs(2),
        "fuel exhaustion must abort quickly, took {:?}: {fuel_err}",
        fuel_start.elapsed()
    );

    // 2. Abundant fuel + short wall-clock limit traps deterministically via epoch interruption.
    let epoch_ext = LoadedExtension::load(&runtime, &wat_path)
        .expect("load spin component")
        .with_limits(RuntimeLimits {
            wasm_fuel: 1_000_000_000,
            invocation_timeout_ms: 150,
            http_timeout_ms: 150,
            ..RuntimeLimits::default()
        })
        .expect("valid epoch limits");
    let (event2, ctx2) = command_event_and_context(&tenant);
    let epoch_start = Instant::now();
    let epoch_err = epoch_ext
        .call_handle_event(
            event2,
            tools,
            ctx2,
            "{}".to_owned(),
            HashSet::new(),
            Vec::new(),
        )
        .await
        .expect_err("spin guest with abundant fuel must trap on epoch deadline");
    assert!(
        epoch_start.elapsed() < Duration::from_secs(2),
        "epoch interruption must bound execution cleanly, took {:?}: {epoch_err}",
        epoch_start.elapsed()
    );
}
