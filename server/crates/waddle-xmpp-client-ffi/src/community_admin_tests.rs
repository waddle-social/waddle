//! FFI test suite for the `urn:waddle:admin:*` XEP-0050 command
//! exports: typed wire mappings for the closed role/affiliation
//! domains and the exported methods' offline failure behavior. The
//! request/response wire shapes themselves are pinned by the shared
//! core suite (`waddle_xmpp_client::admin_commands::tests`).

use std::sync::Arc;

use crate::community_admin::{
    muc_affiliation_from_wire, muc_affiliation_wire, muc_role_from_wire, space_role_from_wire,
    space_role_wire, WaddleSpaceRole,
};
use crate::{
    WaddleAdminChannelsListArgs, WaddleAdminSpacesListArgs, WaddleClient, WaddleClientEvent,
    WaddleConfig, WaddleError, WaddleEventListener, WaddleMucAffiliation, WaddleMucRole,
};

struct NullListener;

impl WaddleEventListener for NullListener {
    fn on_event(&self, _event: WaddleClientEvent) {}
}

fn offline_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "owner@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(NullListener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
    })
}

#[test]
fn space_role_wire_round_trips() {
    for (role, wire) in [
        (WaddleSpaceRole::Owner, "owner"),
        (WaddleSpaceRole::Admin, "admin"),
        (WaddleSpaceRole::Member, "member"),
        (WaddleSpaceRole::None, "none"),
    ] {
        assert_eq!(space_role_wire(role), wire);
        assert_eq!(space_role_from_wire(wire).expect("round trip"), role);
    }
    assert_eq!(
        space_role_from_wire("moderator").unwrap_err(),
        WaddleError::MalformedResponse
    );
}

#[test]
fn affiliation_and_role_wire_mappings_are_typed() {
    assert_eq!(
        muc_affiliation_wire(WaddleMucAffiliation::Outcast),
        "outcast"
    );
    assert!(matches!(
        muc_affiliation_from_wire("owner").expect("parse"),
        WaddleMucAffiliation::Owner
    ));
    assert_eq!(
        muc_affiliation_from_wire("banned").unwrap_err(),
        WaddleError::MalformedResponse
    );
    assert!(matches!(
        muc_role_from_wire("moderator").expect("parse"),
        WaddleMucRole::Moderator
    ));
    assert_eq!(
        muc_role_from_wire("boss").unwrap_err(),
        WaddleError::MalformedResponse
    );
}

#[tokio::test]
async fn is_community_owner_is_false_offline() {
    // The probe is best-effort UI gating: every failure mode
    // (including no connection) must resolve to `false`, never hang
    // or error.
    assert!(!offline_client().is_community_owner().await);
}

#[tokio::test]
async fn admin_users_list_requires_connection() {
    assert_eq!(
        offline_client()
            .admin_users_list(None, Some(50), None)
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
}

#[tokio::test]
async fn admin_v2_commands_require_connection() {
    let client = offline_client();
    assert_eq!(
        client
            .admin_spaces_list(WaddleAdminSpacesListArgs::default())
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
    assert_eq!(
        client
            .admin_channels_list(WaddleAdminChannelsListArgs::default())
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
    assert_eq!(
        client
            .admin_channels_kick(
                "general@muc.waddle.test".to_string(),
                "general@muc.waddle.test/mallory".to_string(),
                None,
            )
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
    assert_eq!(
        client
            .admin_channels_delete("general@muc.waddle.test".to_string())
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
}
