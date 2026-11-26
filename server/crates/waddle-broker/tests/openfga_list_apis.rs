//! Integration tests for OpenFGA `list-objects` and `list-users`, idempotent
//! store discovery, and full `schema 1.2` JSON authorization model shape.

use waddle_broker::{
    openfga_authorization_model_json, AuthzCallerRole, InProcessOpenFgaServer,
    LocalPermissionBackend, OpenFgaConfig, OpenFgaPermissionBackend, OPENFGA_SCHEMA_VERSION,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
};

fn tenant(raw: &str) -> TenantId {
    TenantId::parse(raw).expect("valid tenant id")
}

async fn exercise_list_apis(backend: &dyn PermissionBackend, t: &TenantId) {
    let alice = AccountId::new_v4();
    let bob = AccountId::new_v4();
    let carol = AccountId::new_v4();

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
    let carol_subj = AuthzSubject {
        subject_type: AuthzSubjectType::Account,
        id: carol.to_string(),
        relation: None,
    };

    let space_eng = AuthzObject {
        object_type: AuthzObjectType::Space,
        id: "eng".to_owned(),
    };
    let chan_general = AuthzObject {
        object_type: AuthzObjectType::Channel,
        id: "general".to_owned(),
    };
    let chan_secret = AuthzObject {
        object_type: AuthzObjectType::Channel,
        id: "secret".to_owned(),
    };

    backend
        .write_tuples(
            t,
            AuthzModule::Chat,
            &[
                AuthzTuple {
                    object: space_eng.clone(),
                    relation: AuthzRelation::new("owner"),
                    subject: alice_subj.clone(),
                },
                AuthzTuple {
                    object: space_eng.clone(),
                    relation: AuthzRelation::new("member"),
                    subject: bob_subj.clone(),
                },
                AuthzTuple {
                    object: chan_general.clone(),
                    relation: AuthzRelation::new("parent"),
                    subject: AuthzSubject {
                        subject_type: AuthzSubjectType::Space,
                        id: "eng".to_owned(),
                        relation: None,
                    },
                },
                AuthzTuple {
                    object: chan_general.clone(),
                    relation: AuthzRelation::new("outcast"),
                    subject: bob_subj.clone(),
                },
                AuthzTuple {
                    object: chan_secret.clone(),
                    relation: AuthzRelation::new("member"),
                    subject: carol_subj.clone(),
                },
                AuthzTuple {
                    object: chan_secret.clone(),
                    relation: AuthzRelation::new("admin"),
                    subject: alice_subj.clone(),
                },
            ],
        )
        .await
        .expect("write tuples");

    // Alice can read both `general` (inherited from space owner) and `secret` (direct admin).
    let alice_channels = backend
        .list_objects(
            t,
            &alice_subj,
            &AuthzRelation::new("read"),
            AuthzObjectType::Channel,
        )
        .await
        .expect("list_objects alice");
    assert_eq!(
        alice_channels,
        vec![chan_general.clone(), chan_secret.clone()]
    );

    // Bob is a space member, but an outcast of `general`, so Bob sees zero readable channels.
    let bob_channels = backend
        .list_objects(
            t,
            &bob_subj,
            &AuthzRelation::new("read"),
            AuthzObjectType::Channel,
        )
        .await
        .expect("list_objects bob");
    assert!(bob_channels.is_empty());

    // `list_users` on `general` for `read`: only Alice (Bob is outcast, Carol not member).
    let general_readers = backend
        .list_users(
            t,
            &chan_general,
            &AuthzRelation::new("read"),
            AuthzSubjectType::Account,
        )
        .await
        .expect("list_users general");
    assert_eq!(general_readers, vec![alice_subj.clone()]);

    // `list_users` on `secret` for `read`: both Alice and Carol.
    let mut secret_readers = backend
        .list_users(
            t,
            &chan_secret,
            &AuthzRelation::new("read"),
            AuthzSubjectType::Account,
        )
        .await
        .expect("list_users secret");
    secret_readers.sort_by(|a, b| a.id.cmp(&b.id));
    let mut expected_secret = vec![alice_subj, carol_subj];
    expected_secret.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(secret_readers, expected_secret);
}

#[tokio::test]
async fn openfga_and_local_backends_share_list_objects_and_list_users_parity() {
    let server = InProcessOpenFgaServer::start()
        .await
        .expect("start openfga");
    let openfga = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: server.endpoint().to_owned(),
        bearer_token: Some("fga-test-token".to_owned()),
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("openfga client");
    let local = LocalPermissionBackend::new(AuthzCallerRole::Broker);
    let t = tenant("waddle.social");

    let store_ref = openfga
        .provision_tenant_store(&t)
        .await
        .expect("provision openfga store");
    local
        .provision_tenant_store(&t)
        .await
        .expect("provision local store");

    exercise_list_apis(&openfga, &t).await;
    exercise_list_apis(&local, &t).await;

    // Verify wire contracts on `/list-objects` and `/list-users`.
    let reqs = server.recorded_requests();
    assert!(reqs.iter().any(|r| {
        r.method == "POST"
            && r.path == format!("/stores/{}/list-objects", store_ref.store_id)
            && r.body.as_ref().is_some_and(|b| {
                b["consistency"] == "HIGHER_CONSISTENCY"
                    && b["authorization_model_id"] == store_ref.model_version
                    && b["type"] == "channel"
                    && b["relation"] == "read"
            })
    }));
    assert!(reqs.iter().any(|r| {
        r.method == "POST"
            && r.path == format!("/stores/{}/list-users", store_ref.store_id)
            && r.body.as_ref().is_some_and(|b| {
                b["consistency"] == "HIGHER_CONSISTENCY"
                    && b["authorization_model_id"] == store_ref.model_version
                    && b["object"]["type"] == "channel"
                    && b["relation"] == "read"
                    && b["user_filters"][0]["type"] == "user"
            })
    }));

    // Idempotent store discovery across a fresh client instance (pod restart).
    let restarted = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: server.endpoint().to_owned(),
        bearer_token: Some("fga-test-token".to_owned()),
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("restarted openfga client");
    let discovered = restarted
        .provision_tenant_store(&t)
        .await
        .expect("discover existing store");
    assert_eq!(discovered.store_id, store_ref.store_id);
    assert_eq!(discovered.model_version, store_ref.model_version);

    // Existing tuples remain readable after restart.
    let space_owners = restarted
        .list_users(
            &t,
            &AuthzObject {
                object_type: AuthzObjectType::Space,
                id: "eng".to_owned(),
            },
            &AuthzRelation::new("owner"),
            AuthzSubjectType::Account,
        )
        .await
        .expect("list owners after restart");
    assert_eq!(space_owners.len(), 1);
    let check = restarted
        .check(&CheckRequest {
            tenant_id: t,
            object: AuthzObject {
                object_type: AuthzObjectType::Space,
                id: "eng".to_owned(),
            },
            permission: AuthzRelation::new("manage_settings"),
            subject: space_owners[0].clone(),
            consistency: AuthzConsistency::Strong,
        })
        .await
        .expect("check after restart");
    assert_eq!(check, CheckDecision::Allowed);
}

#[test]
fn openfga_authorization_model_json_declares_schema_1_2_and_relation_metadata() {
    let model = openfga_authorization_model_json();
    assert_eq!(model["schema_version"], OPENFGA_SCHEMA_VERSION);
    let types = model["type_definitions"]
        .as_array()
        .expect("type_definitions array");
    let channel = types
        .iter()
        .find(|t| t["type"] == "channel")
        .expect("channel type");
    assert_eq!(channel["metadata"]["module"], "chat");
    assert!(channel["relations"]["read"].is_object());
    assert!(channel["relations"]["send_message"].is_object());
    assert!(channel["metadata"]["relations"]["parent"]["directly_related_user_types"].is_array());

    let policy = types
        .iter()
        .find(|t| t["type"] == "room_inference_policy")
        .expect("room_inference_policy type");
    assert_eq!(policy["metadata"]["module"], "inference");
    assert!(policy["relations"]["allow_tenant_funded"].is_object());
    assert!(policy["relations"]["allow_member_funded"].is_object());
}
