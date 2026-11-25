//! T2-M4-01: BVA for SpiceDB tenant-scoped raw object ID limit (`MAX_AUTHZ_ID_BYTES = 765`).
//! Exact 765 raw bytes -> accepted (1023 wire bytes <= 1024);
//! 766 raw bytes -> rejected BEFORE any RPC with `IdentifierTooLong { max: 765 }`.

use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError,
};
use waddle_server::permissions::{
    encode_tenant_scoped_id, AuthzCallerRole, AuthzIdError, TenantSpiceDbPermissionBackend,
    MAX_AUTHZ_ID_BYTES, MAX_AUTHZ_WIRE_BYTES,
};

#[tokio::test]
async fn spicedb_tenant_scoped_id_boundary_765_accepted_766_rejected_before_rpc() {
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let prefix_len = tenant.as_str().len() + 1; // "acme.example|"
    let id_765 = "a".repeat(MAX_AUTHZ_ID_BYTES - prefix_len);
    let id_766 = "a".repeat(MAX_AUTHZ_ID_BYTES - prefix_len + 1);

    let encoded_765 = encode_tenant_scoped_id(&tenant, &id_765).expect("765B accepted");
    assert_eq!(encoded_765.as_str().len(), 1023);
    assert!(encoded_765.as_str().len() <= MAX_AUTHZ_WIRE_BYTES);
    assert_eq!(
        encode_tenant_scoped_id(&tenant, &id_766),
        Err(AuthzIdError::TooLong)
    );

    let backend = TenantSpiceDbPermissionBackend::in_memory(AuthzCallerRole::Broker);
    backend
        .provision_tenant_store(&tenant)
        .await
        .expect("provision");
    let alice = AccountId::new_v4();

    // 765-byte scoped ID succeeds.
    let ok_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::Channel,
            id: id_765,
        },
        relation: AuthzRelation::new("member"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: alice.to_string(),
            relation: None,
        },
    };
    backend
        .write_tuples(&tenant, AuthzModule::Chat, std::slice::from_ref(&ok_tuple))
        .await
        .expect("765B write succeeds");
    assert_eq!(
        backend
            .check(&CheckRequest {
                tenant_id: tenant.clone(),
                object: ok_tuple.object,
                permission: AuthzRelation::new("read"),
                subject: ok_tuple.subject.clone(),
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("765B check succeeds"),
        CheckDecision::Allowed
    );

    // 766-byte scoped ID MUST fail BEFORE RPC (rpc_count unchanged!).
    let rpcs_before = backend.rpc_count();
    let bad_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::Channel,
            id: id_766.clone(),
        },
        relation: AuthzRelation::new("member"),
        subject: ok_tuple.subject.clone(),
    };
    assert!(matches!(
        backend
            .write_tuples(&tenant, AuthzModule::Chat, &[bad_tuple])
            .await,
        Err(PermissionBackendError::IdentifierTooLong { max: 765 })
    ));
    assert!(matches!(
        backend
            .check(&CheckRequest {
                tenant_id: tenant,
                object: AuthzObject {
                    object_type: AuthzObjectType::Channel,
                    id: id_766,
                },
                permission: AuthzRelation::new("read"),
                subject: ok_tuple.subject,
                consistency: AuthzConsistency::Strong,
            })
            .await,
        Err(PermissionBackendError::IdentifierTooLong { max: 765 })
    ));
    assert_eq!(
        backend.rpc_count(),
        rpcs_before,
        "oversized ID must be rejected BEFORE any SpiceDB RPC"
    );
}
