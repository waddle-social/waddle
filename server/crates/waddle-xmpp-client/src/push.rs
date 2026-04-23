use crate::{discovery::DiscoveryExt, ClientHandle, ClientResult};

/// XEP-0357 push notifications extension methods.
pub trait PushExt {
    /// Enable push notifications against a push service/node using an APNS device token.
    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        device_token: &str,
    ) -> ClientResult<()>;

    /// Disable push notifications for a previously registered push service/node pair.
    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
    ) -> ClientResult<()>;
}

impl PushExt for ClientHandle {
    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        device_token: &str,
    ) -> ClientResult<()> {
        DiscoveryExt::enable_push_notifications(self, push_service_jid, node, device_token).await
    }

    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
    ) -> ClientResult<()> {
        DiscoveryExt::disable_push_notifications(self, push_service_jid, node).await
    }
}
