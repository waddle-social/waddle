//! T1-F09-05: Reserved heartbeat/cancellation semaphore capacity survives data-plane saturation.

use waddle_broker_core::identity::TenantId;
use waddle_broker_core::quotas::{
    QuotaError, QuotaResourceKind, TenantResourceController, TenantResourceQuotas,
};

#[test]
fn reserved_heartbeat_and_cancellation_capacity_survives_tenant_saturation() {
    let quotas = TenantResourceQuotas {
        max_mailbox_permits: 1,
        max_concurrent_tasks: 1,
        max_outstanding_streams: 1,
        max_wasm_instances: 1,
        max_db_concurrency: 1,
        reserved_control_plane_permits: 2,
        ..TenantResourceQuotas::default()
    };
    let controller = TenantResourceController::new(quotas).expect("valid quotas");
    let noisy = TenantId::parse("noisy.example").expect("valid tenant");

    // Completely saturate all data-plane pools for `noisy`.
    let _mailbox = controller.try_acquire_mailbox(&noisy).expect("mailbox");
    let _task = controller.try_acquire_task(&noisy).expect("task");
    let _stream = controller.try_acquire_stream(&noisy).expect("stream");
    let _db = controller.try_acquire_db(&noisy).expect("db");
    let _wasm = controller
        .try_acquire_wasm_invocation(&noisy, 0)
        .expect("wasm");

    assert!(controller.try_acquire_mailbox(&noisy).is_err());
    assert!(controller.try_acquire_task(&noisy).is_err());
    assert!(controller.try_acquire_stream(&noisy).is_err());
    assert!(controller.try_acquire_db(&noisy).is_err());

    // Control-plane heartbeat & cancellation permits remain unblocked in their own pool.
    let _hb = controller
        .try_acquire_heartbeat(&noisy)
        .expect("reserved heartbeat permit must succeed under full data saturation");
    let _cancel = controller
        .try_acquire_cancellation(&noisy)
        .expect("reserved cancellation permit must succeed under full data saturation");

    assert!(matches!(
        controller.try_acquire_heartbeat(&noisy),
        Err(QuotaError::Exceeded {
            resource: QuotaResourceKind::ControlPlaneHeartbeat,
            ..
        })
    ));
}
