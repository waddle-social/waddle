use std::time::Duration;

use minidom::Element;
use uuid::Uuid;
use waddle_xmpp_client::ClientError;
use xmpp_parsers::{iq::Iq, ping::Ping};

use crate::error::{client_error_to_waddle, WaddleError};
use crate::WaddleClient;

const PING_TIMEOUT: Duration = Duration::from_secs(8);

fn build_ping_iq() -> Element {
    Iq::from_get(Uuid::new_v4().to_string(), Ping).into()
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0199 client-to-server ping used to detect a half-open stream.
    pub async fn ping_server(&self) -> Result<(), WaddleError> {
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };

        let error = match tokio::time::timeout(PING_TIMEOUT, handle.send_iq(build_ping_iq())).await
        {
            Ok(Ok(_)) => return Ok(()),
            // An IQ error is still a timely server response, so the stream
            // is alive even when this server does not support the payload.
            Ok(Err(ClientError::StanzaError(_))) => return Ok(()),
            Ok(Err(error)) => error,
            Err(_) => ClientError::IqTimeout {
                timeout: PING_TIMEOUT,
            },
        };

        self.emit_error(format!("XEP-0199 ping failed: {error}"));
        Err(client_error_to_waddle(&error))
    }
}
