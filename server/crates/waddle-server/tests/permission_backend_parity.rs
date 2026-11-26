//! T1-F14-05: Behavioral parity suite running identical authorization tests
//! against Local, OpenFGA (over HTTP/JSON), and tenant-scoped SpiceDB backends.

use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError,
};
use waddle_server::permissions::{
    AuthzCallerRole, InProcessOpenFgaServer, LocalPermissionBackend, OpenFgaConfig,
    OpenFgaPermissionBackend, TenantSpiceDbPermissionBackend,
};

async fn run_parity_suite<B: PermissionBackend>(broker: &B, cell: &B) {
    let acme = TenantId::parse("acme.example").expect("acme");
    let globex = TenantId::parse("globex.example").expect("globex");

    broker
        .provision_tenant_store(&acme)
        .await
        .expect("prov acme");
    broker
        .provision_tenant_store(&globex)
        .await
        .expect("prov globex");

    let alice = AccountId::new_v4();
    let bob = AccountId::new_v4();

    let space_obj = AuthzObject {
        object_type: AuthzObjectType::Space,
        id: "eng".to_owned(),
    };
    let channel_obj = AuthzObject {
        object_type: AuthzObjectType::Channel,
        id: "general".to_owned(),
    };
    let policy_obj = AuthzObject {
        object_type: AuthzObjectType::RoomInferencePolicy,
        id: "policy-general".to_owned(),
    };
    let alice_subj = AuthzSubject {
        subject_type: AuthzSubjectType::Account,
        id: alice.to_string(),
        relation: None,
    };
    let bob_subj = AuthzSubject {
        subject_type: AuthzSubjectType::Account,
        id: bob.to_string(),
        relation: None,
    };

    // 1. Space membership + channel parent arrow inheritance.
    cell.write_tuples(
        &acme,
        AuthzModule::Chat,
        &[
            AuthzTuple {
                object: space_obj.clone(),
                relation: AuthzRelation::new("member"),
                subject: alice_subj.clone(),
            },
            AuthzTuple {
                object: channel_obj.clone(),
                relation: AuthzRelation::new("space"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Space,
                    id: "eng".to_owned(),
                    relation: None,
                },
            },
        ],
    )
    .await
    .expect("write chat tuples");

    assert_eq!(
        broker
            .check(&CheckRequest {
                tenant_id: acme.clone(),
                object: channel_obj.clone(),
                permission: AuthzRelation::new("read"),
                subject: alice_subj.clone(),
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("check read"),
        CheckDecision::Allowed
    );

    // 2. Room inference policy (broker-only write) + channel outcast precedence.
    let policy_link = AuthzTuple {
        object: policy_obj.clone(),
        relation: AuthzRelation::new("channel"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Channel,
            id: "general".to_owned(),
            relation: None,
        },
    };
    assert!(matches!(
        cell.write_tuples(
            &acme,
            AuthzModule::Inference,
            std::slice::from_ref(&policy_link),
        )
        .await,
        Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference
        ))
    ));
    broker
        .write_tuples(&acme, AuthzModule::Inference, &[policy_link])
        .await
        .expect("broker writes policy");

    assert_eq!(
        broker
            .check(&CheckRequest {
                tenant_id: acme.clone(),
                object: policy_obj.clone(),
                permission: AuthzRelation::new("allow_member_funded"),
                subject: alice_subj.clone(),
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("check allow_member_funded"),
        CheckDecision::Allowed
    );

    // Banning alice as outcast overrides both channel read and room inference policy!
    cell.write_tuples(
        &acme,
        AuthzModule::Chat,
        &[AuthzTuple {
            object: channel_obj.clone(),
            relation: AuthzRelation::new("outcast"),
            subject: alice_subj.clone(),
        }],
    )
    .await
    .expect("ban outcast");

    assert_eq!(
        broker
            .check(&CheckRequest {
                tenant_id: acme.clone(),
                object: channel_obj.clone(),
                permission: AuthzRelation::new("read"),
                subject: alice_subj.clone(),
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("check outcast channel"),
        CheckDecision::Denied
    );
    assert_eq!(
        broker
            .check(&CheckRequest {
                tenant_id: acme.clone(),
                object: policy_obj.clone(),
                permission: AuthzRelation::new("allow_member_funded"),
                subject: alice_subj.clone(),
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("check outcast policy"),
        CheckDecision::Denied
    );

    // 3. replace_exclusive_relation & swap_exclusive_relation (match + mismatch).
    let owner_rel = AuthzRelation::new("owner");
    cell.replace_exclusive_relation(
        &acme,
        AuthzModule::Chat,
        &space_obj,
        &owner_rel,
        &alice_subj,
    )
    .await
    .expect("replace owner");
    assert!(matches!(
        cell.swap_exclusive_relation(
            &acme,
            AuthzModule::Chat,
            &space_obj,
            &owner_rel,
            &bob_subj,
            &alice_subj,
        )
        .await,
        Err(PermissionBackendError::SwapPreconditionFailed)
    ));
    cell.swap_exclusive_relation(
        &acme,
        AuthzModule::Chat,
        &space_obj,
        &owner_rel,
        &alice_subj,
        &bob_subj,
    )
    .await
    .expect("swap owner alice -> bob");
    let owners = broker
        .read_tuples(&acme, &space_obj, Some(&owner_rel))
        .await
        .expect("read owners");
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].subject, bob_subj);

    // 4. Cross-tenant isolation & delete_tenant_store.
    assert_eq!(
        broker
            .check(&CheckRequest {
                tenant_id: globex.clone(),
                object: space_obj.clone(),
                permission: AuthzRelation::new("owner"),
                subject: bob_subj.clone(),
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("globex isolated"),
        CheckDecision::Denied
    );
    broker
        .delete_tenant_store(&acme)
        .await
        .expect("delete acme");
    assert!(matches!(
        broker
            .check(&CheckRequest {
                tenant_id: acme,
                object: space_obj,
                permission: AuthzRelation::new("owner"),
                subject: bob_subj,
                consistency: AuthzConsistency::Strong,
            })
            .await,
        Err(PermissionBackendError::StoreNotProvisioned(_))
    ));
}

#[tokio::test]
async fn permission_backend_parity_across_local_openfga_and_spicedb() {
    // Local
    let local_broker = LocalPermissionBackend::new(AuthzCallerRole::Broker);
    let local_cell = local_broker.with_caller_role(AuthzCallerRole::Cell);
    run_parity_suite(&local_broker, &local_cell).await;

    // OpenFGA (real HTTP/JSON)
    let fga_server = InProcessOpenFgaServer::start()
        .await
        .expect("start openfga");
    let fga_broker = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: fga_server.endpoint().to_owned(),
        bearer_token: None,
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("openfga client");
    let fga_cell = fga_broker.with_caller_role(AuthzCallerRole::Cell);
    run_parity_suite(&fga_broker, &fga_cell).await;

    // SpiceDB (tenant-scoped)
    let spicedb_broker = TenantSpiceDbPermissionBackend::in_memory(AuthzCallerRole::Broker);
    let spicedb_cell = spicedb_broker.with_caller_role(AuthzCallerRole::Cell);
    run_parity_suite(&spicedb_broker, &spicedb_cell).await;
}
