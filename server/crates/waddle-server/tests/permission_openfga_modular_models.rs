//! T1-F14-02: OpenFGA Modular Models (`schema 1.2`) separating
//! `module:<store_id>|chat` from `module:<store_id>|inference`, per-tenant
//! `store_id` isolation, pinned immutable `authorization_model_id`, and `DeleteStore`.

use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError,
};
use waddle_server::permissions::{
    openfga_module_grant_object, openfga_store_grant_object, AuthzCallerRole,
    InProcessOpenFgaServer, OpenFgaConfig, OpenFgaPermissionBackend, OPENFGA_CHAT_MODULE_DSL,
    OPENFGA_INFERENCE_MODULE_DSL, OPENFGA_SCHEMA_VERSION,
};

fn tenant(name: &str) -> TenantId {
    TenantId::parse(name).expect("valid tenant id")
}

#[tokio::test]
async fn openfga_modular_models_enforce_chat_vs_inference_isolation_and_delete_store() {
    assert_eq!(OPENFGA_SCHEMA_VERSION, "1.2");
    assert!(OPENFGA_CHAT_MODULE_DSL.contains("module chat"));
    assert!(OPENFGA_INFERENCE_MODULE_DSL.contains("module inference"));

    let server = InProcessOpenFgaServer::start()
        .await
        .expect("start openfga");
    let broker = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: server.endpoint().to_owned(),
        bearer_token: None,
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("broker openfga client");
    let cell = broker.with_caller_role(AuthzCallerRole::Cell);

    let acme = tenant("acme.example");
    let globex = tenant("globex.example");

    let acme_ref = broker
        .provision_tenant_store(&acme)
        .await
        .expect("provision acme");
    let globex_ref = broker
        .provision_tenant_store(&globex)
        .await
        .expect("provision globex");

    assert_ne!(acme_ref.store_id, globex_ref.store_id);
    assert!(!acme_ref.model_version.is_empty());
    assert_eq!(
        openfga_module_grant_object(&acme_ref.store_id, AuthzModule::Chat),
        format!("module:{}|chat", acme_ref.store_id)
    );
    assert_eq!(
        openfga_module_grant_object(&acme_ref.store_id, AuthzModule::Inference),
        format!("module:{}|inference", acme_ref.store_id)
    );
    assert_eq!(
        openfga_store_grant_object(&acme_ref.store_id),
        format!("store:{}", acme_ref.store_id)
    );

    let alice = AccountId::new_v4();
    let chat_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::Channel,
            id: "general".to_owned(),
        },
        relation: AuthzRelation::new("member"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: alice.to_string(),
            relation: None,
        },
    };
    let inference_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::TenantCredential,
            id: "cred-1".to_owned(),
        },
        relation: AuthzRelation::new("tenant_admin"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: alice.to_string(),
            relation: None,
        },
    };

    // Cell CAN write chat module tuples.
    cell.write_tuples(&acme, AuthzModule::Chat, std::slice::from_ref(&chat_tuple))
        .await
        .expect("cell writes chat");

    // Cell CANNOT write inference module tuples (directly or smuggled under Chat).
    assert!(matches!(
        cell.write_tuples(
            &acme,
            AuthzModule::Inference,
            std::slice::from_ref(&inference_tuple),
        )
        .await,
        Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference
        ))
    ));
    assert!(matches!(
        cell.write_tuples(
            &acme,
            AuthzModule::Chat,
            std::slice::from_ref(&inference_tuple),
        )
        .await,
        Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference
        ))
    ));

    // Broker CAN write inference module tuples.
    broker
        .write_tuples(&acme, AuthzModule::Inference, &[inference_tuple])
        .await
        .expect("broker writes inference");

    // Cross-tenant store isolation: alice has access in acme, denied in globex.
    let acme_decision = broker
        .check(&CheckRequest {
            tenant_id: acme.clone(),
            object: chat_tuple.object.clone(),
            permission: AuthzRelation::new("read"),
            subject: chat_tuple.subject.clone(),
            consistency: AuthzConsistency::Strong,
        })
        .await
        .expect("check acme");
    assert_eq!(acme_decision, CheckDecision::Allowed);

    let globex_decision = broker
        .check(&CheckRequest {
            tenant_id: globex.clone(),
            object: chat_tuple.object.clone(),
            permission: AuthzRelation::new("read"),
            subject: chat_tuple.subject.clone(),
            consistency: AuthzConsistency::Strong,
        })
        .await
        .expect("check globex");
    assert_eq!(globex_decision, CheckDecision::Denied);

    // DeleteStore purges acme's OpenFGA store without touching globex.
    broker
        .delete_tenant_store(&acme)
        .await
        .expect("delete acme store");
    assert!(!server.store_exists(&acme_ref.store_id));
    assert!(server.store_exists(&globex_ref.store_id));
}
