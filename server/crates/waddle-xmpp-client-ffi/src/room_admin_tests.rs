//! Per-XEP FFI test suites for the room lifecycle + member
//! management exports (XEP-0045 §8.2/§9/§10 and XEP-0055).
//!
//! Pins the pure FFI mapping layer (typed enum ↔ wire projections,
//! form → config projection, list-item filtering) plus the exported
//! methods' typed failure behavior (invalid JID, not connected)
//! through an offline client, mirroring `messaging_verbs_tests.rs`.

use std::sync::Arc;

use waddle_xmpp_client::discovery::{MucAdminAffiliationItem, UserSearchItem};
use waddle_xmpp_client::messaging::MucAffiliation;
use waddle_xmpp_client::xep::xep0045_owner::{
    RoomConfigField, RoomConfigForm, FIELD_FORUM_MODE, FIELD_MEMBERS_ONLY, FIELD_PIN_PERMISSION,
    FIELD_ROOM_DESC, FIELD_ROOM_NAME,
};

use crate::room_admin::{
    member_entries_from_items, muc_affiliation_to_core, patch_from_ffi, room_config_from_form,
    user_search_entries_from_items,
};
use crate::{
    WaddleClient, WaddleClientEvent, WaddleConfig, WaddleError, WaddleEventListener,
    WaddleMucAffiliation, WaddlePinPermission, WaddleRoomConfigPatch,
};

struct NullListener;

impl WaddleEventListener for NullListener {
    fn on_event(&self, _event: WaddleClientEvent) {}
}

fn offline_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(NullListener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
    })
}

// ── XEP-0045 §9.5 member lists / §5.2 affiliation changes ────────────

mod xep0045_admin {
    use super::*;

    #[test]
    fn affiliation_enum_round_trips_wire_values() {
        for (ffi, wire) in [
            (WaddleMucAffiliation::Owner, "owner"),
            (WaddleMucAffiliation::Admin, "admin"),
            (WaddleMucAffiliation::Member, "member"),
            (WaddleMucAffiliation::Outcast, "outcast"),
            (WaddleMucAffiliation::None, "none"),
        ] {
            assert_eq!(muc_affiliation_to_core(ffi).as_str(), wire);
        }
    }

    #[test]
    fn member_entries_drop_rows_without_jid_or_affiliation() {
        let items = vec![
            MucAdminAffiliationItem {
                jid: Some("alice@waddle.test".to_string()),
                nick: Some("alice".to_string()),
                affiliation: Some(MucAffiliation::Owner),
                reason: None,
            },
            MucAdminAffiliationItem {
                jid: None,
                nick: Some("ghost".to_string()),
                affiliation: Some(MucAffiliation::Member),
                reason: None,
            },
            MucAdminAffiliationItem {
                jid: Some("mallory@waddle.test".to_string()),
                nick: None,
                affiliation: None,
                reason: None,
            },
            MucAdminAffiliationItem {
                jid: Some("banned@waddle.test".to_string()),
                nick: None,
                affiliation: Some(MucAffiliation::Outcast),
                reason: Some("spam".to_string()),
            },
        ];
        let entries = member_entries_from_items(items);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].jid, "alice@waddle.test");
        assert!(matches!(
            entries[0].affiliation,
            WaddleMucAffiliation::Owner
        ));
        assert_eq!(entries[0].nick.as_deref(), Some("alice"));
        assert_eq!(entries[1].jid, "banned@waddle.test");
        assert!(matches!(
            entries[1].affiliation,
            WaddleMucAffiliation::Outcast
        ));
        assert_eq!(entries[1].reason.as_deref(), Some("spam"));
    }

    #[tokio::test]
    async fn list_room_members_rejects_invalid_room_jid() {
        let client = offline_client();
        let result = client
            .list_room_members("not a jid".to_string(), WaddleMucAffiliation::Member)
            .await;
        assert_eq!(result.unwrap_err(), WaddleError::InvalidJid);
    }

    #[tokio::test]
    async fn set_room_affiliation_requires_connection() {
        let client = offline_client();
        let result = client
            .set_room_affiliation(
                "room@muc.waddle.test".to_string(),
                "bob@waddle.test".to_string(),
                WaddleMucAffiliation::Admin,
                None,
            )
            .await;
        assert_eq!(result.unwrap_err(), WaddleError::NotConnected);
    }

    #[tokio::test]
    async fn kick_occupant_requires_connection_after_jid_parse() {
        let client = offline_client();
        assert_eq!(
            client
                .kick_occupant("bad jid".to_string(), "nick".to_string(), None)
                .await
                .unwrap_err(),
            WaddleError::InvalidJid
        );
        assert_eq!(
            client
                .kick_occupant(
                    "room@muc.waddle.test".to_string(),
                    "nick".to_string(),
                    Some("reason".to_string()),
                )
                .await
                .unwrap_err(),
            WaddleError::NotConnected
        );
    }
}

// ── XEP-0045 §10 owner configuration ─────────────────────────────────

mod xep0045_owner {
    use super::*;

    fn form(fields: Vec<(&str, &str)>) -> RoomConfigForm {
        RoomConfigForm {
            fields: fields
                .into_iter()
                .map(|(var, value)| RoomConfigField {
                    var: var.to_string(),
                    values: vec![value.to_string()],
                })
                .collect(),
        }
    }

    #[test]
    fn room_config_projection_reads_typed_fields() {
        let config = room_config_from_form(&form(vec![
            (FIELD_ROOM_NAME, "General"),
            (FIELD_ROOM_DESC, "All things"),
            (FIELD_MEMBERS_ONLY, "1"),
            (FIELD_FORUM_MODE, "0"),
            (FIELD_PIN_PERMISSION, "anyone"),
        ]));
        assert_eq!(config.name.as_deref(), Some("General"));
        assert_eq!(config.description.as_deref(), Some("All things"));
        assert_eq!(config.members_only, Some(true));
        assert_eq!(config.forum, Some(false));
        assert_eq!(config.public_room, None);
        assert_eq!(config.moderated, None);
        assert!(matches!(
            config.pin_permission,
            Some(WaddlePinPermission::Anyone)
        ));
    }

    #[test]
    fn room_config_projection_treats_unknown_pin_value_as_unset() {
        let config = room_config_from_form(&form(vec![(FIELD_PIN_PERMISSION, "bogus")]));
        assert!(config.pin_permission.is_none());
    }

    #[test]
    fn patch_projection_maps_typed_pin_permission() {
        let patch = patch_from_ffi(WaddleRoomConfigPatch {
            name: Some("Renamed".to_string()),
            description: Some(String::new()),
            forum: Some(true),
            pin_permission: Some(WaddlePinPermission::AdminsOnly),
        });
        assert_eq!(patch.name.as_deref(), Some("Renamed"));
        assert_eq!(patch.description.as_deref(), Some(""));
        assert_eq!(patch.forum, Some(true));
        assert_eq!(
            patch
                .pin_permission
                .expect("pin permission mapped")
                .as_form_value(),
            "admins-only"
        );
    }

    #[tokio::test]
    async fn fetch_room_config_requires_connection() {
        let client = offline_client();
        assert_eq!(
            client
                .fetch_room_config("room@muc.waddle.test".to_string())
                .await
                .unwrap_err(),
            WaddleError::NotConnected
        );
    }

    #[tokio::test]
    async fn submit_room_config_rejects_invalid_room_jid() {
        let client = offline_client();
        assert_eq!(
            client
                .submit_room_config("not a jid".to_string(), WaddleRoomConfigPatch::default())
                .await
                .unwrap_err(),
            WaddleError::InvalidJid
        );
    }

    #[tokio::test]
    async fn create_room_rejects_invalid_localpart() {
        let client = offline_client();
        assert_eq!(
            client
                .create_room(
                    "not@a@localpart".to_string(),
                    "alice".to_string(),
                    WaddleRoomConfigPatch::default(),
                )
                .await
                .unwrap_err(),
            WaddleError::InvalidJid
        );
    }

    #[tokio::test]
    async fn destroy_room_requires_connection() {
        let client = offline_client();
        assert_eq!(
            client
                .destroy_room("room@muc.waddle.test".to_string(), Some("done".to_string()))
                .await
                .unwrap_err(),
            WaddleError::NotConnected
        );
    }
}

// ── XEP-0055 user search ─────────────────────────────────────────────

mod xep0055 {
    use super::*;

    #[test]
    fn search_entries_drop_rows_without_nick() {
        let entries = user_search_entries_from_items(vec![
            UserSearchItem {
                jid: "alice@waddle.test".to_string(),
                nick: Some("alice".to_string()),
                email: None,
                first: Some("Alice".to_string()),
                last: Some("Lidell".to_string()),
            },
            UserSearchItem {
                jid: "noname@waddle.test".to_string(),
                nick: None,
                email: None,
                first: None,
                last: None,
            },
            UserSearchItem {
                jid: "bob@waddle.test".to_string(),
                nick: Some("bob".to_string()),
                email: None,
                first: None,
                last: Some("Builder".to_string()),
            },
        ]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].username, "alice");
        // `first` wins over `last` for the display name (wasm parity).
        assert_eq!(entries[0].display_name.as_deref(), Some("Alice"));
        assert_eq!(entries[1].display_name.as_deref(), Some("Builder"));
    }

    #[tokio::test]
    async fn search_users_requires_connection() {
        let client = offline_client();
        assert_eq!(
            client.search_users("alice".to_string()).await.unwrap_err(),
            WaddleError::NotConnected
        );
    }
}
