//! Shared group-DM membership checks.
//!
//! Group-DM membership is an XEP-0045 `member` affiliation recorded in the
//! Zanzibar permission model (SpiceDB), with a durable mirror in the
//! `permission_tuples` table. Both key on the member's `users.id` — the
//! canonical SpiceDB subject id. SpiceDB object ids forbid `@`/`.`, so a raw JID
//! can never be a valid subject; a requester JID is resolved to its `users.id`
//! before either lookup.

use super::*;

/// Resolve the requester's `users.id`. `Ok(None)` means the requester is not a
/// provisioned account (so cannot be a member); `Err` means the lookup failed
/// and membership is indeterminate.
async fn requester_user_id(
    state: &WebSocketState,
    requester_bare: &BareJid,
) -> Result<Option<String>, crate::auth::AuthError> {
    let Some(localpart) = requester_bare.node().map(|node| node.to_string()) else {
        return Ok(None);
    };
    crate::auth::resolve_user_id(state.deps.app_state.db_pool.global_actor(), &localpart).await
}

/// SpiceDB membership check. `None` is indeterminate (lookup/permission error);
/// `Some(false)` includes "requester is not a provisioned account".
pub(crate) async fn requester_has_group_dm_membership_tuple(
    state: &WebSocketState,
    requester_bare: &BareJid,
    channel_id: &str,
) -> Option<bool> {
    let user_id = match requester_user_id(state, requester_bare).await {
        Ok(Some(user_id)) => user_id,
        Ok(None) => return Some(false),
        Err(_) => return None,
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(user_id),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .map_err(|error| {
            warn!(
                error = %error,
                channel_id,
                "group-DM membership CheckPermission failed"
            );
        })
        .ok()
        .map(|response| response.allowed)
}

/// Durable `permission_tuples` membership fallback.
pub(crate) async fn requester_has_durable_group_dm_membership(
    state: &WebSocketState,
    requester_bare: &BareJid,
    channel_id: &str,
) -> bool {
    let Ok(Some(user_id)) = requester_user_id(state, requester_bare).await else {
        return false;
    };
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbQueryOne {
            sql: r#"
                SELECT 1 FROM permission_tuples
                WHERE object_type = 'channel'
                  AND object_id = ?
                  AND relation = 'member'
                  AND subject_type = 'user'
                  AND subject_id = ?
                  AND subject_relation IS NULL
                LIMIT 1
            "#
            .to_string(),
            params: vec![channel_id.into(), user_id.into()],
        })
        .await
        .is_ok_and(|row| row.is_some())
}
