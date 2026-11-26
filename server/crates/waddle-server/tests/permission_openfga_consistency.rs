//! T1-F14-03: OpenFGA HTTP wire requests carry `"consistency": "HIGHER_CONSISTENCY"`
//! and pinned `"authorization_model_id"` on every `Check`, and omit `contextual_tuples`.

use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
};
use waddle_server::permissions::{
    AuthzCallerRole, InProcessOpenFgaServer, OpenFgaConfig, OpenFgaPermissionBackend,
};

#[tokio::test]
async fn openfga_check_wire_requests_enforce_higher_consistency_and_pinned_model() {
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
    let store_ref = backend
        .provision_tenant_store(&tenant)
        .await
        .expect("provision");

    let alice = AccountId::new_v4();
    let tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::Channel,
            id: "ops".to_owned(),
        },
        relation: AuthzRelation::new("member"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: alice.to_string(),
            relation: None,
        },
    };
    backend
        .write_tuples(&tenant, AuthzModule::Chat, std::slice::from_ref(&tuple))
        .await
        .expect("write");

    let decision = backend
        .check(&CheckRequest {
            tenant_id: tenant.clone(),
            object: tuple.object,
            permission: AuthzRelation::new("read"),
            subject: tuple.subject,
            consistency: AuthzConsistency::Strong,
        })
        .await
        .expect("check");
    assert_eq!(decision, CheckDecision::Allowed);

    let requests = server.recorded_requests();
    let check_reqs: Vec<_> = requests
        .iter()
        .filter(|r| r.path.ends_with("/check"))
        .collect();
    assert_eq!(check_reqs.len(), 1);
    let body = check_reqs[0].body.as_ref().expect("check body");
    assert_eq!(
        body.get("consistency").and_then(|v| v.as_str()),
        Some("HIGHER_CONSISTENCY")
    );
    assert_eq!(
        body.get("authorization_model_id").and_then(|v| v.as_str()),
        Some(store_ref.model_version.as_str())
    );
    assert!(
        body.get("contextual_tuples").is_none(),
        "contextual_tuples must never appear on OpenFGA check wire payload"
    );
}
