//! T2-M2-05: Wasm bounds BVA — `memories * memory_bytes` limit + minimal fuel trap cleanly.

use std::collections::HashSet;
use std::sync::Arc;

use tempfile::tempdir;
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::quotas::TenantMemoryTracker;
use waddle_extensions::config::RuntimeLimits;
use waddle_extensions::host_tools::{DenyingExtensionHostTools, InvocationContext, InvocationKind};
use waddle_extensions::runtime::{LoadedExtension, WasmRuntime};
use waddle_extensions::types::{
    CommandAction, CommandInvocation, CommandNode, ExtensionEvent, FullJidValue,
};
use waddle_extensions::{PluginId, WaddleId};

const BASE_COMPONENT_WAT: &str =
    include_str!("../../waddle-extensions/tests/fixtures/message_hook.wat");

fn grow_memory_wat(extra_pages: u32) -> String {
    let needle = "      i32.const 1024\n    )";
    assert!(BASE_COMPONENT_WAT.contains(needle));
    BASE_COMPONENT_WAT.replacen(
        needle,
        &format!(
            "      (drop (memory.grow (i32.const {extra_pages})))\n      i32.const 1024\n    )"
        ),
        1,
    )
}

fn spin_wat() -> String {
    let needle = "      i32.const 1024\n    )";
    BASE_COMPONENT_WAT.replacen(
        needle,
        "      (loop $spin (br $spin))\n      i32.const 1024\n    )",
        1,
    )
}

fn sample_event_and_ctx(tenant: &TenantId) -> (ExtensionEvent, InvocationContext) {
    let event = ExtensionEvent::Command(CommandInvocation {
        waddle_id: WaddleId::try_new(tenant.as_str()).expect("waddle"),
        room: None,
        requester: FullJidValue::try_new("alice@acme.example/web").expect("requester"),
        command_node: CommandNode::try_new("urn:waddle:extension:1:bva").expect("node"),
        session_id: None,
        action: Some(CommandAction::Execute),
        form: None,
        fields: Vec::new(),
    });
    let ctx = InvocationContext {
        tenant_id: Some(tenant.clone()),
        waddle_id: WaddleId::try_new(tenant.as_str()).expect("waddle"),
        plugin_id: PluginId::try_new("bva-guest").expect("plugin"),
        requester: Some("alice@acme.example".parse().expect("bare")),
        source_room: None,
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    (event, ctx)
}

#[tokio::test]
async fn wasm_memory_limit_plus_one_page_and_minimal_fuel_boundaries() {
    // 1. Validate zero / under-minimum / above-maximum configuration boundaries fail closed.
    let zero_fuel = RuntimeLimits {
        wasm_fuel: 0,
        ..RuntimeLimits::default()
    };
    assert!(zero_fuel.validate().is_err());

    let under_min_mem = RuntimeLimits {
        memory_bytes: (1024 * 1024) - 1,
        ..RuntimeLimits::default()
    };
    assert!(under_min_mem.validate().is_err());

    let zero_timeout = RuntimeLimits {
        invocation_timeout_ms: 0,
        ..RuntimeLimits::default()
    };
    assert!(zero_timeout.validate().is_err());

    // 2. Growing beyond the 1 MiB ceiling (initial 2 pages + 15 extra pages = 17 pages = 1 MiB + 1 page)
    // must fail cleanly via ResourceLimiter and restore TenantMemoryTracker to 0 on store drop.
    let dir = tempdir().expect("tempdir");
    let grow_path = dir.path().join("grow_over_1mib.wat");
    let spin_path = dir.path().join("spin_min_fuel.wat");
    std::fs::write(&grow_path, grow_memory_wat(15)).expect("write grow wat");
    std::fs::write(&spin_path, spin_wat()).expect("write spin wat");

    let runtime = WasmRuntime::try_new().expect("runtime");
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let tracker = Arc::new(TenantMemoryTracker::new(tenant.clone(), 1_048_576));
    let tools = Arc::new(DenyingExtensionHostTools);

    let ok_path = dir.path().join("base_under_1mib.wat");
    std::fs::write(&ok_path, BASE_COMPONENT_WAT).expect("write base wat");
    let ok_ext = LoadedExtension::load(&runtime, &ok_path)
        .expect("load base wat")
        .with_limits(RuntimeLimits {
            memory_bytes: 1_048_576,
            ..RuntimeLimits::default()
        })
        .expect("valid 1 MiB limit");
    let (ev_ok, ctx_ok) = sample_event_and_ctx(&tenant);
    ok_ext
        .call_handle_event_with_memory_tracker(
            ev_ok,
            tools.clone(),
            ctx_ok,
            "{}".to_owned(),
            HashSet::new(),
            Vec::new(),
            Some(Arc::clone(&tracker)),
        )
        .await
        .expect("base 2-page module within 1 MiB ceiling succeeds");
    assert_eq!(tracker.allocated_bytes(), 0);

    let grow_ext = LoadedExtension::load(&runtime, &grow_path)
        .expect("load grow wat")
        .with_limits(RuntimeLimits {
            memory_bytes: 1_048_576,
            ..RuntimeLimits::default()
        })
        .expect("valid 1 MiB limit");
    let (ev1, ctx1) = sample_event_and_ctx(&tenant);
    let grow_err = grow_ext
        .call_handle_event_with_memory_tracker(
            ev1,
            tools.clone(),
            ctx1,
            "{}".to_owned(),
            HashSet::new(),
            Vec::new(),
            Some(Arc::clone(&tracker)),
        )
        .await
        .expect_err("1 MiB + 1 page growth must fail cleanly");
    let msg = grow_err.to_string();
    assert!(
        msg.contains("ResourceLimit") || msg.contains("RuntimeFailure"),
        "unexpected growth error: {msg}"
    );
    assert_eq!(
        tracker.allocated_bytes(),
        0,
        "memory tracker must release all reserved bytes after failed store drops"
    );

    // 3. Minimum valid fuel (1 instruction) traps deterministically.
    let fuel_ext = LoadedExtension::load(&runtime, &spin_path)
        .expect("load spin wat")
        .with_limits(RuntimeLimits {
            wasm_fuel: 1,
            invocation_timeout_ms: 5_000,
            http_timeout_ms: 5_000,
            ..RuntimeLimits::default()
        })
        .expect("valid minimum fuel limits");
    let (ev2, ctx2) = sample_event_and_ctx(&tenant);
    assert!(fuel_ext
        .call_handle_event_with_memory_tracker(
            ev2,
            tools,
            ctx2,
            "{}".to_owned(),
            HashSet::new(),
            Vec::new(),
            None,
        )
        .await
        .is_err());
}
