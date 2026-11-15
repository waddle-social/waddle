use crate::permissions::Permission;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedChannelServerPolicy {
    ChannelOnly,
    DeploymentOwnerOnly,
    DeploymentMembership,
}

pub(crate) fn server_policy_for_managed_channel(
    channel_id: &str,
    permission: &Permission,
) -> ManagedChannelServerPolicy {
    match (channel_id, permission) {
        ("announcements", Permission::SendMessage) => {
            ManagedChannelServerPolicy::DeploymentOwnerOnly
        }
        (
            "chat" | "github-actions",
            Permission::View | Permission::Read | Permission::SendMessage,
        )
        | ("announcements", Permission::View | Permission::Read) => {
            ManagedChannelServerPolicy::DeploymentMembership
        }
        _ => ManagedChannelServerPolicy::ChannelOnly,
    }
}

pub(crate) const DEPLOYMENT_MEMBERSHIP_PERMISSIONS: [Permission; 3] =
    [Permission::Owner, Permission::Admin, Permission::Member];

/// The host-grant action a channel permission check asks for, in a room an
/// enrolled host-grant scope manages. A relation name (`owner`, `member`,
/// `outcast`, ...) or any other permission has none: such a check is not
/// decided by a room's grants and is refused there.
pub(crate) fn host_grant_channel_action(
    permission: &Permission,
) -> Option<crate::permissions::host_grant::HostGrantAction> {
    use crate::permissions::host_grant::HostGrantAction;
    match permission {
        Permission::View => Some(HostGrantAction::View),
        Permission::Read => Some(HostGrantAction::Read),
        Permission::SendMessage => Some(HostGrantAction::SendMessage),
        Permission::Moderate => Some(HostGrantAction::Moderate),
        Permission::Manage => Some(HostGrantAction::Manage),
        Permission::Delete => Some(HostGrantAction::Delete),
        _ => None,
    }
}

/// A channel permission check for a room an enrolled host-grant scope
/// manages, decided from its canonical grants: `Ok(None)` when no scope
/// manages the channel and the legacy check applies, `Err` when that cannot
/// be read. The deployment-wide fallbacks of the default channels do not
/// apply to such a room.
pub(crate) async fn host_grant_channel_permission(
    host_grants: &crate::permissions::host_grant::HostGrantGate,
    subject: &str,
    channel_id: &str,
    permission: &Permission,
) -> Result<Option<bool>, String> {
    use crate::permissions::host_grant::{RoomBinding, RoomGate};
    match host_grants.room(RoomBinding::Channel(channel_id)).await {
        RoomGate::Unmanaged => Ok(None),
        RoomGate::Unavailable => Err("the host-grant authority is unavailable".to_string()),
        RoomGate::Managed(room) => {
            let Some(action) = host_grant_channel_action(permission) else {
                return Ok(Some(false));
            };
            let Ok(subject) = subject.parse::<jid::BareJid>() else {
                return Ok(Some(false));
            };
            Ok(Some(room.allows(&subject, action)))
        }
    }
}
