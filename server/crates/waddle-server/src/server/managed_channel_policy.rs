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
