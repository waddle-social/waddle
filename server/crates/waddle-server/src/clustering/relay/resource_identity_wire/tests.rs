use super::super::{
    NodeId, RelayActor, RelayRegisterRemoteUserResource, RelayRemoteResourceRegistrationReply,
    RelayRemoteResourceRegistrationStatus, RelayTraceContext, RelayUnregisterRemoteUserResource,
    RelayUpdateRemoteUserResource, RemoteResourceRegistrationId, RemoteResourceSocketGeneration,
    RemoteResourceStateSnapshot, RemoteResourceStateUpdate,
};
use super::{
    RelayRegisterRemoteUserResourceV5, RelayRemoteResourceRegistrationV5Reply,
    RelayRemoteResourceRegistrationV5Result, RelayUnregisterRemoteUserResourceV3,
    RelayUpdateRemoteUserResourceV2, RemoteResourceStateSnapshotV5, REMOTE_RESOURCE_REGISTER_V5_ID,
    REMOTE_RESOURCE_UNREGISTER_V3_ID, REMOTE_RESOURCE_UPDATE_V2_ID,
};
use crate::clustering::route_bridge::RemotePresenceStateSnapshot;
use kameo::remote::RemoteMessage;
use serde::de::DeserializeOwned;
use serde::Serialize;
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ConnectionRef, NodeIdentity, NodeLeaseIdentity, UserClaimIdentity,
};

const CONNECTION_REF_FIXTURE: &str = r#"{"node_lease":{"Clustered":{"node_id":"node-a","node_epoch":"epoch-a"}},"transport_generation":"00000000-0000-0000-0000-000000000001"}"#;
const BINDING_REF_FIXTURE: &str = r#"{"connection_ref":{"node_lease":{"Clustered":{"node_id":"node-a","node_epoch":"epoch-a"}},"transport_generation":"00000000-0000-0000-0000-000000000001"},"attempt_id":"00000000-0000-0000-0000-000000000002"}"#;

fn connection_ref() -> ConnectionRef {
    serde_json::from_str(CONNECTION_REF_FIXTURE).expect("connection reference fixture")
}

fn binding_ref() -> BindingRef {
    serde_json::from_str(BINDING_REF_FIXTURE).expect("binding reference fixture")
}

fn claim_identity() -> UserClaimIdentity {
    UserClaimIdentity::fresh(
        "alice@example.test".parse().expect("bare JID"),
        NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("owner-a", "lease-a")),
        ClaimEpoch(7),
    )
}

fn registration_id() -> RemoteResourceRegistrationId {
    serde_json::from_str("\"00000000-0000-0000-0000-000000000003\"")
        .expect("registration ID fixture")
}

fn socket_generation() -> RemoteResourceSocketGeneration {
    serde_json::from_str("3").expect("socket generation fixture")
}

fn legacy_state() -> RemoteResourceStateSnapshot {
    RemoteResourceStateSnapshot {
        connection_ref: connection_ref(),
        occupancy_session: None,
        carbons_enabled: true,
        roster_interested: false,
        blocklist_interested: true,
        presence_available: true,
        presence_priority: 4,
        presence_state: None,
    }
}

fn legacy_register() -> RelayRegisterRemoteUserResource {
    RelayRegisterRemoteUserResource {
        jid: "alice@example.test/phone".parse().expect("full JID"),
        registration_id: registration_id(),
        socket_generation: socket_generation(),
        socket_node: NodeId::new("socket-a".to_owned()),
        state: legacy_state(),
        trace: RelayTraceContext::default(),
    }
}

fn legacy_update() -> RelayUpdateRemoteUserResource {
    RelayUpdateRemoteUserResource {
        jid: "alice@example.test/phone".parse().expect("full JID"),
        registration_id: registration_id(),
        socket_generation: socket_generation(),
        update: RemoteResourceStateUpdate::Carbons { enabled: false },
        trace: RelayTraceContext::default(),
    }
}

fn legacy_unregister() -> RelayUnregisterRemoteUserResource {
    RelayUnregisterRemoteUserResource {
        jid: "alice@example.test/phone".parse().expect("full JID"),
        registration_id: registration_id(),
        socket_generation: socket_generation(),
        trace: RelayTraceContext::default(),
    }
}

fn identity_register(binding_ref: BindingRef) -> RelayRegisterRemoteUserResourceV5 {
    RelayRegisterRemoteUserResourceV5 {
        jid: "alice@example.test/phone".parse().expect("full JID"),
        registration_id: registration_id(),
        socket_generation: socket_generation(),
        socket_node: NodeId::new("socket-a".to_owned()),
        state: RemoteResourceStateSnapshotV5 {
            binding_ref,
            occupancy_session: None,
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: true,
            presence_available: true,
            presence_priority: 4,
            presence_state: Some(RemotePresenceStateSnapshot {
                show: None,
                status: Some("ready".to_owned()),
                priority: 4,
                payloads: Vec::new(),
            }),
        },
        trace: RelayTraceContext::default(),
    }
}

fn identity_update(
    user_claim_identity: UserClaimIdentity,
    binding_ref: BindingRef,
) -> RelayUpdateRemoteUserResourceV2 {
    RelayUpdateRemoteUserResourceV2 {
        jid: "alice@example.test/phone".parse().expect("full JID"),
        registration_id: registration_id(),
        socket_generation: socket_generation(),
        user_claim_identity,
        binding_ref,
        update: RemoteResourceStateUpdate::Carbons { enabled: false },
        trace: RelayTraceContext::default(),
    }
}

fn identity_unregister(
    user_claim_identity: UserClaimIdentity,
    binding_ref: BindingRef,
) -> RelayUnregisterRemoteUserResourceV3 {
    RelayUnregisterRemoteUserResourceV3 {
        jid: "alice@example.test/phone".parse().expect("full JID"),
        registration_id: registration_id(),
        socket_generation: socket_generation(),
        user_claim_identity,
        binding_ref,
        trace: RelayTraceContext::default(),
    }
}

fn assert_json_round_trip<T>(message: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_vec(message).expect("encode identity message");
    let decoded: T = serde_json::from_slice(&encoded).expect("decode identity message");
    assert_eq!(&decoded, message);
}

fn assert_named_messagepack_round_trip<T>(message: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = rmp_serde::to_vec_named(message).expect("encode named MessagePack message");
    let decoded: T = rmp_serde::from_slice(&encoded).expect("decode named MessagePack message");
    assert_eq!(&decoded, message);
}

fn messagepack_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn identity_bearing_resource_messages_round_trip_exact_claim_and_binding_identity() {
    let binding_ref = binding_ref();
    let register = identity_register(binding_ref.clone());
    assert_eq!(
        register.state.binding_ref.connection_ref(),
        &connection_ref()
    );
    assert_json_round_trip(&register);
    assert_named_messagepack_round_trip(&register);

    let identity = claim_identity();
    let update = identity_update(identity.clone(), binding_ref.clone());
    assert_json_round_trip(&update);
    assert_named_messagepack_round_trip(&update);

    let unregister = identity_unregister(identity.clone(), binding_ref.clone());
    assert_json_round_trip(&unregister);
    assert_named_messagepack_round_trip(&unregister);

    let reply = RelayRemoteResourceRegistrationV5Reply {
        result: RelayRemoteResourceRegistrationV5Result::Registered {
            user_claim_identity: identity,
        },
    };
    assert_json_round_trip(&reply);
    assert_named_messagepack_round_trip(&reply);
}

#[test]
fn failed_resume_and_fresh_bind_on_the_same_transport_have_distinct_binding_refs() {
    let connection = connection_ref();
    let failed_resume = BindingRef::fresh(connection.clone());
    let fresh_bind = BindingRef::fresh(connection.clone());

    assert_eq!(
        failed_resume.connection_ref(),
        fresh_bind.connection_ref(),
        "both attempts came from the same physical socket"
    );
    assert_ne!(
        failed_resume.attempt_id(),
        fresh_bind.attempt_id(),
        "the fresh bind must not reuse the failed resume attempt"
    );

    let decoded: BindingRef = serde_json::from_slice(
        &serde_json::to_vec(&failed_resume).expect("encode failed resume binding"),
    )
    .expect("decode failed resume binding");
    assert_eq!(decoded, failed_resume);
    assert_ne!(decoded, fresh_bind);

    let messagepack_decoded: BindingRef = rmp_serde::from_slice(
        &rmp_serde::to_vec_named(&failed_resume).expect("encode failed resume binding"),
    )
    .expect("decode failed resume binding");
    assert_eq!(messagepack_decoded, failed_resume);
    assert_ne!(messagepack_decoded, fresh_bind);
}

#[test]
fn identity_endpoints_use_new_ids_and_legacy_payloads_do_not_decode_as_identity_messages() {
    assert_eq!(
        <RelayActor as RemoteMessage<RelayRegisterRemoteUserResource>>::REMOTE_ID,
        "waddle.clustering.relay.remote_resource_register.v4"
    );
    assert_eq!(
        <RelayActor as RemoteMessage<RelayUpdateRemoteUserResource>>::REMOTE_ID,
        "waddle.clustering.relay.remote_resource_update.v1"
    );
    assert_eq!(
        <RelayActor as RemoteMessage<RelayUnregisterRemoteUserResource>>::REMOTE_ID,
        "waddle.clustering.relay.remote_resource_unregister.v2"
    );
    assert_eq!(
        REMOTE_RESOURCE_REGISTER_V5_ID,
        "waddle.clustering.relay.remote_resource_register.v5"
    );
    assert_eq!(
        REMOTE_RESOURCE_UPDATE_V2_ID,
        "waddle.clustering.relay.remote_resource_update.v2"
    );
    assert_eq!(
        REMOTE_RESOURCE_UNREGISTER_V3_ID,
        "waddle.clustering.relay.remote_resource_unregister.v3"
    );
    assert_ne!(
        <RelayActor as RemoteMessage<RelayRegisterRemoteUserResource>>::REMOTE_ID,
        REMOTE_RESOURCE_REGISTER_V5_ID
    );
    assert_ne!(
        <RelayActor as RemoteMessage<RelayUpdateRemoteUserResource>>::REMOTE_ID,
        REMOTE_RESOURCE_UPDATE_V2_ID
    );
    assert_ne!(
        <RelayActor as RemoteMessage<RelayUnregisterRemoteUserResource>>::REMOTE_ID,
        REMOTE_RESOURCE_UNREGISTER_V3_ID
    );

    let legacy_register = serde_json::to_value(legacy_register()).expect("legacy register");
    let legacy_update = serde_json::to_value(legacy_update()).expect("legacy update");
    let legacy_unregister = serde_json::to_value(legacy_unregister()).expect("legacy unregister");
    let legacy_reply = serde_json::to_value(RelayRemoteResourceRegistrationReply {
        status: RelayRemoteResourceRegistrationStatus::Registered,
    })
    .expect("legacy reply");
    let identity_register = RelayRegisterRemoteUserResourceV5 {
        jid: "alice@example.test/phone".parse().expect("full JID"),
        registration_id: registration_id(),
        socket_generation: socket_generation(),
        socket_node: NodeId::new("socket-a".to_owned()),
        state: RemoteResourceStateSnapshotV5 {
            binding_ref: binding_ref(),
            occupancy_session: None,
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: true,
            presence_available: true,
            presence_priority: 4,
            presence_state: None,
        },
        trace: RelayTraceContext::default(),
    };
    let identity_register = serde_json::to_value(identity_register).expect("identity register");

    assert!(serde_json::from_value::<RelayRegisterRemoteUserResourceV5>(legacy_register).is_err());
    assert!(serde_json::from_value::<RelayUpdateRemoteUserResourceV2>(legacy_update).is_err());
    assert!(
        serde_json::from_value::<RelayUnregisterRemoteUserResourceV3>(legacy_unregister).is_err()
    );
    assert!(
        serde_json::from_value::<RelayRemoteResourceRegistrationV5Reply>(legacy_reply).is_err()
    );
    assert!(
        serde_json::from_value::<RelayRegisterRemoteUserResource>(identity_register).is_err(),
        "the v5 binding state cannot decode as a v4 connection-only snapshot"
    );
}

#[test]
fn serde_tolerance_does_not_make_legacy_endpoint_ids_support_identity_messages() {
    let identity = claim_identity();
    let update = RelayUpdateRemoteUserResourceV2 {
        jid: "alice@example.test/phone".parse().expect("full JID"),
        registration_id: registration_id(),
        socket_generation: socket_generation(),
        user_claim_identity: identity.clone(),
        binding_ref: binding_ref(),
        update: RemoteResourceStateUpdate::Carbons { enabled: false },
        trace: RelayTraceContext::default(),
    };
    let unregister = RelayUnregisterRemoteUserResourceV3 {
        jid: update.jid.clone(),
        registration_id: update.registration_id,
        socket_generation: update.socket_generation,
        user_claim_identity: identity,
        binding_ref: update.binding_ref.clone(),
        trace: RelayTraceContext::default(),
    };

    // Serde structs normally ignore unknown fields, so codec tolerance alone
    // is not a capability signal. The distinct remote message IDs above are
    // the boundary; these v2/v3 endpoints have no registered handlers yet.
    assert!(serde_json::from_value::<RelayUpdateRemoteUserResource>(
        serde_json::to_value(update).expect("encode v2 update")
    )
    .is_ok());
    assert!(serde_json::from_value::<RelayUnregisterRemoteUserResource>(
        serde_json::to_value(unregister).expect("encode v3 unregister")
    )
    .is_ok());
    assert_ne!(
        <RelayActor as RemoteMessage<RelayUpdateRemoteUserResource>>::REMOTE_ID,
        REMOTE_RESOURCE_UPDATE_V2_ID
    );
    assert_ne!(
        <RelayActor as RemoteMessage<RelayUnregisterRemoteUserResource>>::REMOTE_ID,
        REMOTE_RESOURCE_UNREGISTER_V3_ID
    );
}

#[test]
fn named_messagepack_version_evolution_preserves_only_intended_decoder_tolerance() {
    let legacy_register_bytes =
        rmp_serde::to_vec_named(&legacy_register()).expect("encode legacy register");
    let identity_register_bytes = rmp_serde::to_vec_named(&identity_register(binding_ref()))
        .expect("encode identity register");
    assert!(
        rmp_serde::from_slice::<RelayRegisterRemoteUserResourceV5>(&legacy_register_bytes).is_err(),
        "a v4 connection-only registration has no required v5 binding reference"
    );
    assert!(
        rmp_serde::from_slice::<RelayRegisterRemoteUserResource>(&identity_register_bytes).is_err(),
        "a v5 binding-only snapshot does not satisfy the v4 connection-only snapshot"
    );

    let legacy_update_bytes = rmp_serde::to_vec_named(&legacy_update()).expect("encode v1 update");
    let identity_update_bytes =
        rmp_serde::to_vec_named(&identity_update(claim_identity(), binding_ref()))
            .expect("encode v2 update");
    assert!(
        rmp_serde::from_slice::<RelayUpdateRemoteUserResourceV2>(&legacy_update_bytes).is_err(),
        "v2 requires claim and binding identities"
    );
    assert!(
        rmp_serde::from_slice::<RelayUpdateRemoteUserResource>(&identity_update_bytes).is_ok(),
        "the old decoder ignores added identity fields, so message IDs remain the capability boundary"
    );

    let legacy_unregister_bytes =
        rmp_serde::to_vec_named(&legacy_unregister()).expect("encode v2 unregister");
    let identity_unregister_bytes =
        rmp_serde::to_vec_named(&identity_unregister(claim_identity(), binding_ref()))
            .expect("encode v3 unregister");
    assert!(
        rmp_serde::from_slice::<RelayUnregisterRemoteUserResourceV3>(&legacy_unregister_bytes)
            .is_err(),
        "v3 requires claim and binding identities"
    );
    assert!(
        rmp_serde::from_slice::<RelayUnregisterRemoteUserResource>(&identity_unregister_bytes)
            .is_ok(),
        "the old decoder can ignore added fields but is never registered under the v3 ID"
    );

    let legacy_reply = RelayRemoteResourceRegistrationReply {
        status: RelayRemoteResourceRegistrationStatus::Registered,
    };
    let legacy_reply_bytes = rmp_serde::to_vec_named(&legacy_reply).expect("encode v4 reply");
    let identity_reply = RelayRemoteResourceRegistrationV5Reply {
        result: RelayRemoteResourceRegistrationV5Result::Registered {
            user_claim_identity: claim_identity(),
        },
    };
    let identity_reply_bytes = rmp_serde::to_vec_named(&identity_reply).expect("encode v5 reply");
    assert!(
        rmp_serde::from_slice::<RelayRemoteResourceRegistrationV5Reply>(&legacy_reply_bytes)
            .is_err(),
        "a status-only v4 success reply cannot provide v5 claim identity"
    );
    assert!(
        rmp_serde::from_slice::<RelayRemoteResourceRegistrationReply>(&identity_reply_bytes)
            .is_err(),
        "the v5 result shape does not satisfy the legacy status reply"
    );
}

#[test]
fn legacy_resource_registration_wire_bytes_remain_frozen() {
    let register = serde_json::to_string(&legacy_register()).expect("encode legacy register");
    let update = serde_json::to_string(&legacy_update()).expect("encode legacy update");
    let unregister = serde_json::to_string(&legacy_unregister()).expect("encode legacy unregister");
    let register_messagepack =
        rmp_serde::to_vec_named(&legacy_register()).expect("encode legacy register MessagePack");
    let update_messagepack =
        rmp_serde::to_vec_named(&legacy_update()).expect("encode legacy update MessagePack");
    let unregister_messagepack = rmp_serde::to_vec_named(&legacy_unregister())
        .expect("encode legacy unregister MessagePack");

    assert_eq!(
        messagepack_hex(&register_messagepack),
        "86a36a6964b8616c696365406578616d706c652e746573742f70686f6e65af726567697374726174696f6e5f6964c41000000000000000000000000000000003b1736f636b65745f67656e65726174696f6e03ab736f636b65745f6e6f6465a8736f636b65742d61a5737461746588ae636f6e6e656374696f6e5f72656682aa6e6f64655f6c6561736581a9436c7573746572656482a76e6f64655f6964a66e6f64652d61aa6e6f64655f65706f6368a765706f63682d61b47472616e73706f72745f67656e65726174696f6ec41000000000000000000000000000000001b16f63637570616e63795f73657373696f6ec0af636172626f6e735f656e61626c6564c3b1726f737465725f696e7465726573746564c2b4626c6f636b6c6973745f696e7465726573746564c3b270726573656e63655f617661696c61626c65c3b170726573656e63655f7072696f7269747904ae70726573656e63655f7374617465c0a5747261636580",
        "baseline v4 named-MessagePack bytes"
    );
    assert_eq!(
        messagepack_hex(&update_messagepack),
        "85a36a6964b8616c696365406578616d706c652e746573742f70686f6e65af726567697374726174696f6e5f6964c41000000000000000000000000000000003b1736f636b65745f67656e65726174696f6e03a675706461746581a7436172626f6e7381a7656e61626c6564c2a5747261636580",
        "baseline v1 named-MessagePack bytes"
    );
    assert_eq!(
        messagepack_hex(&unregister_messagepack),
        "84a36a6964b8616c696365406578616d706c652e746573742f70686f6e65af726567697374726174696f6e5f6964c41000000000000000000000000000000003b1736f636b65745f67656e65726174696f6e03a5747261636580",
        "baseline v2 named-MessagePack bytes"
    );

    assert_eq!(
        register,
        r#"{"jid":"alice@example.test/phone","registration_id":"00000000-0000-0000-0000-000000000003","socket_generation":3,"socket_node":"socket-a","state":{"connection_ref":{"node_lease":{"Clustered":{"node_id":"node-a","node_epoch":"epoch-a"}},"transport_generation":"00000000-0000-0000-0000-000000000001"},"occupancy_session":null,"carbons_enabled":true,"roster_interested":false,"blocklist_interested":true,"presence_available":true,"presence_priority":4,"presence_state":null},"trace":{}}"#
    );
    assert_eq!(
        update,
        r#"{"jid":"alice@example.test/phone","registration_id":"00000000-0000-0000-0000-000000000003","socket_generation":3,"update":{"Carbons":{"enabled":false}},"trace":{}}"#
    );
    assert_eq!(
        unregister,
        r#"{"jid":"alice@example.test/phone","registration_id":"00000000-0000-0000-0000-000000000003","socket_generation":3,"trace":{}}"#
    );
}
