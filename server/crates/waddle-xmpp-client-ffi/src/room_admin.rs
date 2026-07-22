//! Room lifecycle + member management exports: XEP-0045 §9.5 member
//! lists, §5.2 affiliation changes (ban = §9.1 outcast), §8.2 kicks
//! (role → none), §10.1/§10.2 owner configuration, §10.9 destroy, and
//! the XEP-0055 user search backing the add-member flow.
//!
//! Every method parses its untyped FFI inputs (JID strings, enum
//! values) into typed values immediately, then delegates to the
//! shared `waddle-xmpp-client` builders — the same code paths the
//! wasm chat client drives. All methods here are fetch-style
//! `Result<_, WaddleError>` surfaces: member management UIs need the
//! stanza-error condition (`forbidden`, `not-allowed`, `conflict`) to
//! explain *why* an action was refused.

use jid::BareJid;

use waddle_xmpp_client::discovery::{
    build_muc_admin_affiliation_list_iq, build_muc_admin_affiliation_set_iq,
    build_muc_admin_role_set_iq, build_user_search_iq, parse_muc_admin_affiliation_query,
    parse_user_search_result, DiscoveryExt, MucAdminAffiliationItem, UserSearchQuery,
};
use waddle_xmpp_client::messaging::{MessagingExt, MucRole};
use waddle_xmpp_client::xep::xep0045_owner::{
    apply_room_config_patch, build_muc_owner_config_get_iq, build_muc_owner_config_submit_iq,
    build_muc_owner_destroy_iq, parse_muc_owner_config_form, RoomConfigForm, RoomConfigPatch,
    RoomPinPermission, FIELD_FORUM_MODE, FIELD_MEMBERS_ONLY, FIELD_MODERATED_ROOM,
    FIELD_PIN_PERMISSION, FIELD_PUBLIC_ROOM, FIELD_ROOM_DESC, FIELD_ROOM_NAME,
};

use crate::boundary_convert::jid_domain;
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError, WaddleMucAffiliation};

// ── Typed FFI payloads ───────────────────────────────────────────────

/// One entry of a XEP-0045 §9.5 affiliation list.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleRoomMemberEntry {
    /// Bare JID of the affiliated user.
    pub jid: String,
    pub affiliation: WaddleMucAffiliation,
    /// Reserved room nickname, when the service reports one.
    pub nick: Option<String>,
    /// Reason recorded with the affiliation change (e.g. ban reason).
    pub reason: Option<String>,
}

/// `urn:waddle:roomconfig:pinpermission` policy values.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddlePinPermission {
    AdminsOnly,
    Anyone,
}

impl From<WaddlePinPermission> for RoomPinPermission {
    fn from(value: WaddlePinPermission) -> Self {
        match value {
            WaddlePinPermission::AdminsOnly => RoomPinPermission::AdminsOnly,
            WaddlePinPermission::Anyone => RoomPinPermission::Anyone,
        }
    }
}

/// Current owner-visible room configuration, projected from the
/// XEP-0045 §10.2 config form. Every field is optional: `None` means
/// the service did not offer that field (or reported no value), so
/// UIs can distinguish "unset" from an explicit `false`/empty.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleRoomConfig {
    pub name: Option<String>,
    pub description: Option<String>,
    pub members_only: Option<bool>,
    pub public_room: Option<bool>,
    pub moderated: Option<bool>,
    /// Waddle forum-mode flag (`muc#roomconfig_forum`, pre-existing
    /// registrar-prefix debt carried for parity).
    pub forum: Option<bool>,
    pub pin_permission: Option<WaddlePinPermission>,
}

/// Owner edit patch for [`WaddleClient::submit_room_config`]. `None`
/// keeps the value the service currently reports (§10.2 GET-merge-SET
/// round-trips unedited fields verbatim). `description: Some("")`
/// clears the description.
#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct WaddleRoomConfigPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub forum: Option<bool>,
    pub pin_permission: Option<WaddlePinPermission>,
}

/// XEP-0055 user directory search hit (`jabber:iq:search`).
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleUserSearchEntry {
    pub jid: String,
    /// The `<nick/>` column — Waddle usernames.
    pub username: String,
    pub display_name: Option<String>,
}

pub(crate) fn muc_affiliation_to_core(
    value: WaddleMucAffiliation,
) -> waddle_xmpp_client::messaging::MucAffiliation {
    use waddle_xmpp_client::messaging::MucAffiliation;
    match value {
        WaddleMucAffiliation::Owner => MucAffiliation::Owner,
        WaddleMucAffiliation::Admin => MucAffiliation::Admin,
        WaddleMucAffiliation::Member => MucAffiliation::Member,
        WaddleMucAffiliation::Outcast => MucAffiliation::Outcast,
        WaddleMucAffiliation::None => MucAffiliation::None,
    }
}

/// Project §9.5 affiliation-list items into FFI entries, dropping
/// rows that lack a JID or affiliation (web parity — such rows are
/// unrenderable).
pub(crate) fn member_entries_from_items(
    items: Vec<MucAdminAffiliationItem>,
) -> Vec<WaddleRoomMemberEntry> {
    items
        .into_iter()
        .filter_map(|item| {
            let jid = item.jid?;
            let affiliation = item.affiliation?;
            Some(WaddleRoomMemberEntry {
                jid,
                affiliation: crate::convert::muc_affiliation_to_ffi(affiliation),
                nick: item.nick,
                reason: item.reason,
            })
        })
        .collect()
}

/// Project XEP-0055 search items into FFI entries; rows without a
/// `<nick/>` (the Waddle username column) are dropped (wasm parity).
pub(crate) fn user_search_entries_from_items(
    items: Vec<waddle_xmpp_client::discovery::UserSearchItem>,
) -> Vec<WaddleUserSearchEntry> {
    items
        .into_iter()
        .filter_map(|item| {
            let username = item.nick?;
            Some(WaddleUserSearchEntry {
                jid: item.jid,
                username,
                display_name: item.first.or(item.last),
            })
        })
        .collect()
}

pub(crate) fn room_config_from_form(form: &RoomConfigForm) -> WaddleRoomConfig {
    WaddleRoomConfig {
        name: form.value(FIELD_ROOM_NAME).map(str::to_string),
        description: form.value(FIELD_ROOM_DESC).map(str::to_string),
        members_only: form.bool_value(FIELD_MEMBERS_ONLY),
        public_room: form.bool_value(FIELD_PUBLIC_ROOM),
        moderated: form.bool_value(FIELD_MODERATED_ROOM),
        forum: form.bool_value(FIELD_FORUM_MODE),
        pin_permission: form
            .value(FIELD_PIN_PERMISSION)
            .and_then(RoomPinPermission::from_form_value)
            .map(|value| match value {
                RoomPinPermission::AdminsOnly => WaddlePinPermission::AdminsOnly,
                RoomPinPermission::Anyone => WaddlePinPermission::Anyone,
            }),
    }
}

pub(crate) fn patch_from_ffi(patch: WaddleRoomConfigPatch) -> RoomConfigPatch {
    RoomConfigPatch {
        name: patch.name,
        description: patch.description,
        forum: patch.forum,
        pin_permission: patch.pin_permission.map(RoomPinPermission::from),
    }
}

impl WaddleClient {
    fn require_bare_jid(&self, value: &str) -> Result<BareJid, WaddleError> {
        value
            .parse::<BareJid>()
            .map_err(|_| WaddleError::InvalidJid)
    }

    /// Fetch + parse the §10.2 owner config form for `room`.
    async fn fetch_room_config_form(&self, room: &BareJid) -> Result<RoomConfigForm, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_muc_owner_config_get_iq(room);
        let result = send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        parse_muc_owner_config_form(&result).ok_or(WaddleError::MalformedResponse)
    }

    /// §10.2 GET-merge-SET: fetch the current form, apply `patch`,
    /// submit the merged form back.
    async fn configure_room(
        &self,
        room: &BareJid,
        patch: RoomConfigPatch,
    ) -> Result<(), WaddleError> {
        let mut form = self.fetch_room_config_form(room).await?;
        apply_room_config_patch(&mut form, &patch);
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_muc_owner_config_submit_iq(room, &form);
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(())
    }
}

// ── Exported verbs ───────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0045 §9.5: retrieve the affiliation list for one tier.
    /// Callers query the four tiers (owner/admin/member/outcast)
    /// separately and tolerate per-tier `forbidden` /
    /// `service-unavailable` errors — web `MucAdmin.listRoomMembers`
    /// parity.
    pub async fn list_room_members(
        &self,
        room_jid: String,
        affiliation: WaddleMucAffiliation,
    ) -> Result<Vec<WaddleRoomMemberEntry>, WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_muc_admin_affiliation_list_iq(
            room.as_str(),
            muc_affiliation_to_core(affiliation),
        );
        let result = send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(member_entries_from_items(
            parse_muc_admin_affiliation_query(&result).unwrap_or_default(),
        ))
    }

    /// XEP-0045 §5.2 affiliation change addressed by bare JID:
    /// promote/demote (member/admin/owner), remove (`none`), or ban
    /// (§9.1 `outcast`). The service enforces the admin/owner
    /// privilege matrix and answers `forbidden`/`not-allowed`
    /// otherwise.
    pub async fn set_room_affiliation(
        &self,
        room_jid: String,
        target_jid: String,
        affiliation: WaddleMucAffiliation,
        reason: Option<String>,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let target = self.require_bare_jid(&target_jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let item = MucAdminAffiliationItem {
            jid: Some(target.to_string()),
            nick: None,
            affiliation: Some(muc_affiliation_to_core(affiliation)),
            reason: reason.filter(|reason| !reason.is_empty()),
        };
        let iq = build_muc_admin_affiliation_set_iq(room.as_str(), &[item]);
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(())
    }

    /// XEP-0045 §8.2 kick: eject the occupant with nickname `nick` by
    /// setting `role='none'`. Distinct from a ban — the target's
    /// affiliation is untouched and they may rejoin.
    pub async fn kick_occupant(
        &self,
        room_jid: String,
        nick: String,
        reason: Option<String>,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq =
            build_muc_admin_role_set_iq(room.as_str(), &nick, MucRole::None, reason.as_deref());
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(())
    }

    /// XEP-0055 user search against the account domain's
    /// `jabber:iq:search` directory, matching on the `nick` column
    /// (wasm `search_users` parity). Backs the members add-member
    /// flow.
    pub async fn search_users(
        &self,
        query: String,
    ) -> Result<Vec<WaddleUserSearchEntry>, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let domain = jid_domain(&self.config.jid).to_string();
        let iq = build_user_search_iq(
            &domain,
            &UserSearchQuery {
                nick: Some(query),
                ..UserSearchQuery::default()
            },
        );
        let result = send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(parse_user_search_result(&result)
            .map(|results| user_search_entries_from_items(results.items))
            .unwrap_or_default())
    }

    /// XEP-0045 §10.2: fetch the owner configuration form and project
    /// it into the typed [`WaddleRoomConfig`]. Owner-only; the
    /// service answers `forbidden` for everyone else.
    pub async fn fetch_room_config(
        &self,
        room_jid: String,
    ) -> Result<WaddleRoomConfig, WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let form = self.fetch_room_config_form(&room).await?;
        Ok(room_config_from_form(&form))
    }

    /// XEP-0045 §10.2 GET-merge-SET: apply `patch` on top of the
    /// service's current form and submit the merged result, so
    /// unedited settings round-trip verbatim.
    pub async fn submit_room_config(
        &self,
        room_jid: String,
        patch: WaddleRoomConfigPatch,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        self.configure_room(&room, patch_from_ffi(patch)).await
    }

    /// XEP-0045 §10.1 room creation: join `localpart@<muc service>`
    /// with `nick` (creating the room), then run the §10.1.3/§10.2
    /// reserved-room sequence — fetch the config form, merge the
    /// initial `patch`, submit — and leave again best-effort (web
    /// `createMucRoom` parity: the caller joins properly through the
    /// normal flow afterwards). Returns the bare room JID.
    ///
    /// If `localpart` collides with an existing room the creator does
    /// not own, the config fetch fails with a `forbidden` stanza
    /// error — surfaced verbatim so the UI can report the conflict.
    pub async fn create_room(
        &self,
        localpart: String,
        nick: String,
        patch: WaddleRoomConfigPatch,
    ) -> Result<String, WaddleError> {
        let node = jid::NodePart::new(localpart.trim()).map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let domain = jid_domain(&self.config.jid).to_string();
        let services = handle
            .discover_component_services(&domain)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        let room = BareJid::from_parts(Some(&node), services.muc.domain());
        handle
            .join_room(room.as_str(), &nick)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        let configure = self.configure_room(&room, patch_from_ffi(patch)).await;
        // Best-effort leave regardless of configure outcome; the
        // caller re-joins through the regular topology flow.
        let _ = handle.leave_room(room.as_str(), &nick).await;
        configure?;
        Ok(room.to_string())
    }

    /// XEP-0045 §10.9: destroy the room. Owner-only server-side.
    pub async fn destroy_room(
        &self,
        room_jid: String,
        reason: Option<String>,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let iq = build_muc_owner_destroy_iq(&room, reason.as_deref());
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        Ok(())
    }
}
