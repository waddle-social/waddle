use crate::error::ConnectionError;

pub struct ConnectionConfig {
    pub jid: String,
    pub password: String,
    pub server: Option<String>,
    pub port: Option<u16>,
    pub timeout_seconds: u32,
    pub max_reconnect_attempts: u32,
}

/// Platform-abstracted XMPP transport.
///
/// Feature-gated implementations provide the concrete transport:
/// - `NativeTcpTransport` (native feature): TCP/TLS via tokio-xmpp + rustls
/// - `WebSocketTransport` (web feature): WebSocket via tokio-tungstenite / web-sys
pub trait XmppTransport: Send + 'static {
    fn connect(
        config: &ConnectionConfig,
    ) -> impl Future<Output = Result<Self, ConnectionError>> + Send
    where
        Self: Sized;

    fn send(&mut self, data: &[u8]) -> impl Future<Output = Result<(), ConnectionError>> + Send;

    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, ConnectionError>> + Send;

    fn close(&mut self) -> impl Future<Output = Result<(), ConnectionError>> + Send;
}

#[cfg(feature = "native")]
mod native {
    use super::*;

    pub struct NativeTcpTransport {
        _private: (),
    }

    impl XmppTransport for NativeTcpTransport {
        async fn connect(_config: &ConnectionConfig) -> Result<Self, ConnectionError> {
            todo!("native TCP/TLS transport connect")
        }

        async fn send(&mut self, _data: &[u8]) -> Result<(), ConnectionError> {
            todo!("native TCP/TLS transport send")
        }

        async fn recv(&mut self) -> Result<Vec<u8>, ConnectionError> {
            todo!("native TCP/TLS transport recv")
        }

        async fn close(&mut self) -> Result<(), ConnectionError> {
            todo!("native TCP/TLS transport close")
        }
    }
}

#[cfg(feature = "web")]
mod web {
    use super::*;

    pub struct WebSocketTransport {
        _private: (),
    }

    impl XmppTransport for WebSocketTransport {
        async fn connect(_config: &ConnectionConfig) -> Result<Self, ConnectionError> {
            todo!("WebSocket transport connect")
        }

        async fn send(&mut self, _data: &[u8]) -> Result<(), ConnectionError> {
            todo!("WebSocket transport send")
        }

        async fn recv(&mut self) -> Result<Vec<u8>, ConnectionError> {
            todo!("WebSocket transport recv")
        }

        async fn close(&mut self) -> Result<(), ConnectionError> {
            todo!("WebSocket transport close")
        }
    }
}

#[cfg(feature = "native")]
pub use native::NativeTcpTransport;

#[cfg(feature = "web")]
pub use web::WebSocketTransport;
