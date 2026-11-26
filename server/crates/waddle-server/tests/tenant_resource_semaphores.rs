//! T1-F09-02: Per-tenant mailbox/task/stream/DB semaphores isolate tenant load.

use waddle_broker_core::identity::TenantId;
use waddle_broker_core::quotas::{
    QuotaError, QuotaResourceKind, TenantResourceController, TenantResourceQuotas,
};

#[test]
fn per_tenant_mailbox_task_stream_db_semaphores_isolate_load() {
    let quotas = TenantResourceQuotas {
        max_mailbox_permits: 2,
        max_concurrent_tasks: 2,
        max_outstanding_streams: 2,
        max_wasm_instances: 2,
        max_db_concurrency: 2,
        reserved_control_plane_permits: 2,
        ..TenantResourceQuotas::default()
    };
    let controller = TenantResourceController::new(quotas).expect("valid quotas");
    let noisy = TenantId::parse("noisy.example").expect("valid noisy tenant");
    let victim = TenantId::parse("victim.example").expect("valid victim tenant");

    let _m1 = controller.try_acquire_mailbox(&noisy).expect("mailbox 1");
    let _m2 = controller.try_acquire_mailbox(&noisy).expect("mailbox 2");
    assert!(matches!(
        controller.try_acquire_mailbox(&noisy),
        Err(QuotaError::Exceeded {
            resource: QuotaResourceKind::Mailbox,
            ..
        })
    ));

    let _t1 = controller.try_acquire_task(&noisy).expect("task 1");
    let _t2 = controller.try_acquire_task(&noisy).expect("task 2");
    assert!(matches!(
        controller.try_acquire_task(&noisy),
        Err(QuotaError::Exceeded {
            resource: QuotaResourceKind::BackgroundTask,
            ..
        })
    ));

    let _s1 = controller.try_acquire_stream(&noisy).expect("stream 1");
    let _s2 = controller.try_acquire_stream(&noisy).expect("stream 2");
    assert!(matches!(
        controller.try_acquire_stream(&noisy),
        Err(QuotaError::Exceeded {
            resource: QuotaResourceKind::Stream,
            ..
        })
    ));

    let _d1 = controller.try_acquire_db(&noisy).expect("db 1");
    let _d2 = controller.try_acquire_db(&noisy).expect("db 2");
    assert!(matches!(
        controller.try_acquire_db(&noisy),
        Err(QuotaError::Exceeded {
            resource: QuotaResourceKind::Database,
            ..
        })
    ));

    let _w1 = controller
        .try_acquire_wasm_invocation(&noisy, 0)
        .expect("wasm 1");
    let _w2 = controller
        .try_acquire_wasm_invocation(&noisy, 0)
        .expect("wasm 2");
    assert!(matches!(
        controller.try_acquire_wasm_invocation(&noisy, 0),
        Err(QuotaError::Exceeded {
            resource: QuotaResourceKind::WasmInstance,
            ..
        })
    ));

    // Victim tenant retains full independent permit capacity across all four+one pools.
    let vm = controller
        .try_acquire_mailbox(&victim)
        .expect("victim mailbox unaffected");
    let vt = controller
        .try_acquire_task(&victim)
        .expect("victim task unaffected");
    let vs = controller
        .try_acquire_stream(&victim)
        .expect("victim stream unaffected");
    let vd = controller
        .try_acquire_db(&victim)
        .expect("victim db unaffected");
    let vw = controller
        .try_acquire_wasm_invocation(&victim, 0)
        .expect("victim wasm unaffected");
    drop((vm, vt, vs, vd, vw));

    // Releasing noisy's permits restores noisy's capacity.
    drop(_m1);
    assert!(controller.try_acquire_mailbox(&noisy).is_ok());
}
