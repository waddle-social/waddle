use std::future::Future;
use std::time::Duration;

use minidom::Element;
use uuid::Uuid;
use waddle_xmpp_client::ClientError;
use xmpp_parsers::{iq::Iq, ping::Ping};

use crate::error::{client_error_to_waddle, WaddleError};
use crate::WaddleClient;

const PING_TIMEOUT: Duration = Duration::from_secs(8);

pub(super) fn build_ping_iq() -> Element {
    Iq::from_get(Uuid::new_v4().to_string(), Ping).into()
}

/// Send one XEP-0199 ping and treat any timely stanza-level response as
/// proof that the stream is alive. The generic sender keeps the request
/// construction and timeout behavior testable without a live transport.
pub(super) async fn send_ping<F, Fut>(send_iq: F, timeout: Duration) -> Result<(), ClientError>
where
    F: FnOnce(Element) -> Fut,
    Fut: Future<Output = Result<Element, ClientError>>,
{
    match tokio::time::timeout(timeout, send_iq(build_ping_iq())).await {
        Ok(Ok(_)) => Ok(()),
        // An IQ error is still a timely server response, so the stream
        // is alive even when this server does not support the payload.
        Ok(Err(ClientError::StanzaError(_))) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(ClientError::IqTimeout { timeout }),
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0199 client-to-server ping used to detect a half-open stream.
    pub async fn ping_server(&self) -> Result<(), WaddleError> {
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };

        if let Err(error) = send_ping(|iq| handle.send_iq(iq), PING_TIMEOUT).await {
            self.emit_error(format!("XEP-0199 ping failed: {error}"));
            return Err(client_error_to_waddle(&error));
        }

        Ok(())
    }
}
