use jid::BareJid;
use serde::Deserialize;
use url::Url;
use waddle_xmpp_client_ffi::WaddleConfig;

use crate::error::RuntimeError;
use crate::generated::Operation as Command;

/// Credentials are never Debug or Serialize, and remain private to one client generation.
#[derive(Deserialize)]
pub(crate) struct ConnectArgs {
    server_url: Url,
    jid: BareJid,
    access_token: String,
    resource: String,
    #[serde(default)]
    resumable: bool,
}

impl ConnectArgs {
    pub(crate) fn into_config(self) -> Result<WaddleConfig, RuntimeError> {
        if !matches!(self.server_url.scheme(), "wss" | "ws") {
            return Err(RuntimeError::invalid("A WebSocket URL is required"));
        }
        if self.resource.trim().is_empty() || self.access_token.is_empty() {
            return Err(RuntimeError::invalid(
                "Credentials and a resource are required",
            ));
        }
        // This independent app deliberately does not persist stream-resumption state yet.
        if self.resumable {
            return Err(RuntimeError::invalid(
                "Stream resumption is not supported by this runtime",
            ));
        }
        Ok(WaddleConfig {
            server_url: self.server_url.to_string(),
            jid: self.jid.to_string(),
            access_token: self.access_token,
            resource: self.resource,
            resumable: false,
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum Request {
    Client(Command),
    Timeline(waddle_flutter_timeline::TimelineCommand),
}
