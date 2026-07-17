use waddle_xmpp_client::discovery::DiscoveryExt;

use crate::{
    WaddleClient, WaddlePushDeviceCredentials, WaddlePushEnvironment, WaddleRegisterDeviceResult,
};

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0357 §5 `<enable/>` IQ against the user's XMPP server.
    /// Never carries provider credentials — those flow through
    /// `register_push_device` (XEP-0050) at `push.<domain>`.
    pub async fn enable_push_notifications(&self, push_service_jid: String, node: String) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle
            .enable_push_notifications(&push_service_jid, &node, None)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                self.emit_error(format!("enable_push_notifications failed: {e}"));
                false
            }
        }
    }

    /// XEP-0357 §6.1 `<disable/>` IQ. A `None`/missing `node` disables
    /// ALL push nodes at the service for this user.
    pub async fn disable_push_notifications(
        &self,
        push_service_jid: String,
        node: Option<String>,
    ) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle
            .disable_push_notifications(&push_service_jid, node.as_deref())
            .await
        {
            Ok(()) => true,
            Err(e) => {
                self.emit_error(format!("disable_push_notifications failed: {e}"));
                false
            }
        }
    }

    /// XEP-0050 `register-device` ad-hoc command on `push.<domain>`.
    /// Drives the multi-step dance and returns the assigned
    /// [`WaddleRegisterDeviceResult`] (node id + device id) on
    /// success. Returns `None` on failure with the diagnostic on the
    /// event stream. The caller MUST persist both fields — node feeds
    /// the user-server XEP-0357 `<enable/>` IQ, device id scopes the
    /// per-device `disable_push_device` opt-out.
    pub async fn register_push_device(
        &self,
        push_service_jid: String,
        app_id: String,
        environment: WaddlePushEnvironment,
        credentials: WaddlePushDeviceCredentials,
    ) -> Option<WaddleRegisterDeviceResult> {
        let handle = self.clone_handle().await?;
        let env: waddle_xmpp_client::push::PushEnvironment = environment.into();
        let creds: waddle_xmpp_client::push::PushDeviceCredentials = credentials.into();
        let push_service_jid =
            match waddle_xmpp_client::push::PushServiceJid::new(&push_service_jid) {
                Ok(value) => value,
                Err(e) => {
                    self.emit_error(format!("register_push_device failed: {e}"));
                    return None;
                }
            };
        let app_id = match waddle_xmpp_client::push::PushAppId::new(&app_id) {
            Ok(value) => value,
            Err(e) => {
                self.emit_error(format!("register_push_device failed: {e}"));
                return None;
            }
        };
        match waddle_xmpp_client::push::register_push_device(
            &handle,
            &push_service_jid,
            &app_id,
            env,
            &creds,
        )
        .await
        {
            Ok(outcome) => Some(WaddleRegisterDeviceResult {
                node: outcome.node.into_string(),
                device_id: outcome.device_id.into_string(),
            }),
            Err(e) => {
                self.emit_error(format!("register_push_device failed: {e}"));
                None
            }
        }
    }

    /// XEP-0050 `disable-device` ad-hoc command on `push.<domain>`.
    /// Per-device scope — `device_id` is the value returned by the
    /// preceding [`register_push_device`] call. Sibling devices on
    /// the same node keep receiving fan-out. Returns `true` when the
    /// command completes (including the idempotent already-disabled
    /// case).
    pub async fn disable_push_device(
        &self,
        push_service_jid: String,
        node: String,
        device_id: String,
    ) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        let form = waddle_xmpp_client::push::build_disable_device_submit_form(&node, &device_id);
        let iq = waddle_xmpp_client::xep::xep0050::build_xep0050_command_request(
            &push_service_jid,
            waddle_xmpp_client::push::DISABLE_DEVICE_NODE,
            waddle_xmpp_client::xep::xep0050::AdHocAction::Execute,
            Some(form),
        );
        match handle.send_iq(iq).await {
            Ok(_) => true,
            Err(e) => {
                self.emit_error(format!("disable_push_device failed: {e}"));
                false
            }
        }
    }
}
