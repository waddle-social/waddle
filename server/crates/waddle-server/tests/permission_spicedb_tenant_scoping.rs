//! T1-F14-04: SpiceDB tenant-scoped IDs, `AUTHZ_SCHEMA_VERSION == 3`,
//! inference definitions in `SPICEDB_SCHEMA`, and module write isolation.

use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError,
};
use waddle_server::permissions::{
    builtin_authz_schema, decode_tenant_scoped_id, encode_tenant_scoped_id, AuthzCallerRole,
    TenantSpiceDbPermissionBackend, AUTHZ_SCHEMA_VERSION,
};

#[tokio::test]
async fn spicedb_tenant_scoping_and_schema_v3_isolation() {
    assert_eq!(AUTHZ_SCHEMA_VERSION, 3);
    let schema = builtin_authz_schema();
    assert!(schema.contains("relation version_3: waddle_authz_schema"));
    assert!(schema.contains("definition tenant_credential"));
    assert!(schema.contains("definition user_credential"));
    assert!(schema.contains("definition installed_extension"));
    assert!(schema.contains("definition room_inference_policy"));

    let acme = TenantId::parse("acme.example").expect("acme");
    let globex = TenantId::parse("globex.example").expect("globex");

    // Injective tenant-scoped wire encoding.
    let wire_acme = encode_tenant_scoped_id(&acme, "room-1").expect("encode acme");
    let wire_globex = encode_tenant_scoped_id(&globex, "room-1").expect("encode globex");
    assert_ne!(wire_acme.as_str(), wire_globex.as_str());
    assert_eq!(
        decode_tenant_scoped_id(wire_acme.as_str()).expect("decode acme"),
        ("acme.example".to_owned(), "room-1".to_owned())
    );

    let broker = TenantSpiceDbPermissionBackend::in_memory(AuthzCallerRole::Broker);
    let cell = broker.with_caller_role(AuthzCallerRole::Cell);
    let acme_ref = broker
        .provision_tenant_store(&acme)
        .await
        .expect("provision acme");
    let _globex_ref = broker
        .provision_tenant_store(&globex)
        .await
        .expect("provision globex");
    assert_eq!(acme_ref.model_version, "spicedb-schema-v3");

    let alice = AccountId::new_v4();
    let cred_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::UserCredential,
            id: "ucred-1".to_owned(),
        },
        relation: AuthzRelation::new("owner"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: alice.to_string(),
            relation: None,
        },
    };

    // Cell cannot write inference tuples.
    assert!(matches!(
        cell.write_tuples(
            &acme,
            AuthzModule::Inference,
            std::slice::from_ref(&cred_tuple),
        )
        .await,
        Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference
        ))
    ));

    // Broker writes inference tuple in acme; globex cannot observe it.
    broker
        .write_tuples(
            &acme,
            AuthzModule::Inference,
            std::slice::from_ref(&cred_tuple),
        )
        .await
        .expect("broker writes inference");

    assert_eq!(
        broker
            .check(&CheckRequest {
                tenant_id: acme.clone(),
                object: cred_tuple.object.clone(),
                permission: AuthzRelation::new("use"),
                subject: cred_tuple.subject.clone(),
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("check acme"),
        CheckDecision::Allowed
    );
    assert_eq!(
        broker
            .check(&CheckRequest {
                tenant_id: globex,
                object: cred_tuple.object,
                permission: AuthzRelation::new("use"),
                subject: cred_tuple.subject,
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("check globex"),
        CheckDecision::Denied
    );
}
