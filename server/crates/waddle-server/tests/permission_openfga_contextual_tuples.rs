//! T2-M4-02: BVA for OpenFGA contextual_tuples rejection (1 tuple, N tuples)
//! BEFORE RPC and prohibition of `MINIMIZE_LATENCY` / `UNSPECIFIED` consistency.

use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, CheckDecision, CheckRequest, PermissionBackend, PermissionBackendError,
};
use waddle_server::permissions::{
    parse_openfga_consistency, AuthzCallerRole, InProcessOpenFgaServer, OpenFgaConfig,
    OpenFgaConsistency, OpenFgaPermissionBackend,
};

#[tokio::test]
async fn openfga_rejects_contextual_tuples_before_rpc_and_prohibits_weak_consistency() {
    assert_eq!(
        parse_openfga_consistency("HIGHER_CONSISTENCY").expect("higher allowed"),
        OpenFgaConsistency::HigherConsistency
    );
    assert!(parse_openfga_consistency("MINIMIZE_LATENCY").is_err());
    assert!(parse_openfga_consistency("UNSPECIFIED").is_err());

    let server = InProcessOpenFgaServer::start()
        .await
        .expect("start openfga");
    let backend = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: server.endpoint().to_owned(),
        bearer_token: None,
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("openfga client");

    let tenant = TenantId::parse("acme.example").expect("tenant");
    backend
        .provision_tenant_store(&tenant)
        .await
        .expect("provision");

    let alice = AccountId::new_v4();
    let req = CheckRequest {
        tenant_id: tenant.clone(),
        object: AuthzObject {
            object_type: AuthzObjectType::Channel,
            id: "secret-room".to_owned(),
        },
        permission: AuthzRelation::new("read"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: alice.to_string(),
            relation: None,
        },
        consistency: AuthzConsistency::Strong,
    };

    // Empty contextual_tuples -> RPC executes and returns Denied.
    assert_eq!(
        backend
            .check_with_contextual_tuples(&req, &[])
            .await
            .expect("empty contextual_tuples ok"),
        CheckDecision::Denied
    );
    let count_before = server.recorded_requests().len();

    // 1 contextual tuple -> rejected BEFORE RPC.
    let spoof_tuple = AuthzTuple {
        object: req.object.clone(),
        relation: AuthzRelation::new("member"),
        subject: req.subject.clone(),
    };
    assert!(matches!(
        backend
            .check_with_contextual_tuples(&req, std::slice::from_ref(&spoof_tuple))
            .await,
        Err(PermissionBackendError::ContextualTuplesForbidden)
    ));

    // Multiple contextual tuples -> rejected BEFORE RPC.
    assert!(matches!(
        backend
            .check_with_contextual_tuples(&req, &[spoof_tuple.clone(), spoof_tuple])
            .await,
        Err(PermissionBackendError::ContextualTuplesForbidden)
    ));

    assert_eq!(
        server.recorded_requests().len(),
        count_before,
        "contextual_tuples must be rejected BEFORE any HTTP RPC"
    );
}
