//! XML stream handling for XMPP connections.

use jid::{BareJid, FullJid};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, instrument};

use crate::connection::Stanza;
use crate::XmppError;

/// XMPP stream handler.
///
/// Manages the XML stream lifecycle including STARTTLS upgrade,
/// SASL authentication, and stanza reading/writing.
pub struct XmppStream {
    /// The underlying stream (either TCP or TLS)
    inner: StreamInner,
}

enum StreamInner {
    None,
    Tcp(TcpStream),
    Tls(TlsStream<TcpStream>),
}

impl Default for StreamInner {
    fn default() -> Self {
        StreamInner::None
    }
}

impl XmppStream {
    /// Create a new XMPP stream from a TCP connection.
    pub fn new(stream: TcpStream) -> Self {
        Self {
            inner: StreamInner::Tcp(stream),
        }
    }

    /// Read bytes from the underlying stream.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, XmppError> {
        match &mut self.inner {
            StreamInner::None => Err(XmppError::internal("Stream not initialized")),
            StreamInner::Tcp(s) => Ok(s.read(buf).await?),
            StreamInner::Tls(s) => Ok(s.read(buf).await?),
        }
    }

    /// Write bytes to the underlying stream.
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), XmppError> {
        match &mut self.inner {
            StreamInner::None => Err(XmppError::internal("Stream not initialized")),
            StreamInner::Tcp(s) => Ok(s.write_all(buf).await?),
            StreamInner::Tls(s) => Ok(s.write_all(buf).await?),
        }
    }

    /// Read the XMPP stream header from the client.
    #[instrument(skip(self), name = "xmpp.stream.read_header")]
    pub async fn read_stream_header(&mut self) -> Result<(), XmppError> {
        // Read until we have a complete stream header
        // For now, simplified implementation
        let mut buf = [0u8; 4096];
        let n = self.read(&mut buf).await?;

        if n == 0 {
            return Err(XmppError::stream("Connection closed during header"));
        }

        let data = String::from_utf8_lossy(&buf[..n]);
        debug!(data = %data, "Received stream header");

        // Validate it looks like an XMPP stream header
        if !data.contains("<stream:stream") && !data.contains("<?xml") {
            return Err(XmppError::xml_parse("Invalid stream header"));
        }

        // Send our stream header response
        let response = format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' \
            xmlns:stream='http://etherx.jabber.org/streams' \
            id='{}' from='localhost' version='1.0'>",
            uuid::Uuid::new_v4()
        );

        self.write_all(response.as_bytes()).await?;

        Ok(())
    }

    /// Send stream features advertising STARTTLS.
    #[instrument(skip(self), name = "xmpp.stream.send_features_starttls")]
    pub async fn send_features_starttls(&mut self) -> Result<(), XmppError> {
        let features = "\
            <stream:features>\
                <starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'>\
                    <required/>\
                </starttls>\
            </stream:features>";

        self.write_all(features.as_bytes()).await?;

        debug!("Sent STARTTLS features");
        Ok(())
    }

    /// Handle STARTTLS upgrade.
    #[instrument(skip(self, tls_acceptor), name = "xmpp.stream.starttls")]
    pub async fn handle_starttls(&mut self, tls_acceptor: TlsAcceptor) -> Result<(), XmppError> {
        // Read STARTTLS request
        let mut buf = [0u8; 1024];
        let n = match &mut self.inner {
            StreamInner::None => return Err(XmppError::internal("Stream not initialized")),
            StreamInner::Tcp(s) => s.read(&mut buf).await?,
            StreamInner::Tls(_) => return Err(XmppError::stream("Already using TLS")),
        };

        if n == 0 {
            return Err(XmppError::stream("Connection closed during STARTTLS"));
        }

        let data = String::from_utf8_lossy(&buf[..n]);
        if !data.contains("<starttls") {
            return Err(XmppError::stream("Expected STARTTLS"));
        }

        // Send proceed
        let proceed = "<proceed xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>";
        match &mut self.inner {
            StreamInner::None => return Err(XmppError::internal("Stream not initialized")),
            StreamInner::Tcp(s) => s.write_all(proceed.as_bytes()).await?,
            StreamInner::Tls(_) => return Err(XmppError::stream("Already using TLS")),
        }

        // Upgrade to TLS - take ownership of the TCP stream
        let tcp_stream = match std::mem::take(&mut self.inner) {
            StreamInner::Tcp(s) => s,
            StreamInner::Tls(_) => return Err(XmppError::stream("Already using TLS")),
            StreamInner::None => return Err(XmppError::internal("Stream already taken")),
        };

        let tls_stream = tls_acceptor
            .accept(tcp_stream)
            .await
            .map_err(|e| XmppError::internal(format!("TLS accept error: {}", e)))?;

        self.inner = StreamInner::Tls(tls_stream);
        debug!("TLS upgrade complete");

        Ok(())
    }

    /// Send stream features advertising SASL mechanisms.
    #[instrument(skip(self), name = "xmpp.stream.send_features_sasl")]
    pub async fn send_features_sasl(&mut self) -> Result<(), XmppError> {
        let features = "\
            <stream:features>\
                <mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>\
                    <mechanism>PLAIN</mechanism>\
                </mechanisms>\
            </stream:features>";

        self.write_all(features.as_bytes()).await?;

        debug!("Sent SASL features");
        Ok(())
    }

    /// Handle SASL authentication.
    ///
    /// Returns the authenticated JID and token.
    #[instrument(skip(self), name = "xmpp.stream.authenticate")]
    pub async fn handle_sasl_auth(&mut self) -> Result<(BareJid, String), XmppError> {
        let mut buf = [0u8; 4096];
        let n = self.read(&mut buf).await?;

        if n == 0 {
            return Err(XmppError::stream("Connection closed during SASL"));
        }

        let data = String::from_utf8_lossy(&buf[..n]);
        debug!(data = %data, "Received SASL auth");

        // Parse SASL PLAIN: base64(authzid \0 authcid \0 password)
        // For our use case: base64(jid \0 token)
        let (jid, token) = self.parse_sasl_plain(&data)?;

        // Send success (actual validation happens in the connection actor)
        let success = "<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>";
        self.write_all(success.as_bytes()).await?;

        Ok((jid, token))
    }

    /// Parse SASL PLAIN authentication data.
    fn parse_sasl_plain(&self, data: &str) -> Result<(BareJid, String), XmppError> {
        // Extract base64 content
        let start = data.find('>').ok_or_else(|| XmppError::xml_parse("Invalid SASL auth"))? + 1;
        let end = data.rfind('<').ok_or_else(|| XmppError::xml_parse("Invalid SASL auth"))?;
        let b64 = &data[start..end];

        // Decode base64
        use base64::prelude::*;
        let decoded = BASE64_STANDARD
            .decode(b64)
            .map_err(|e| XmppError::auth_failed(format!("Invalid base64: {}", e)))?;

        // Parse \0-separated parts
        let parts: Vec<&[u8]> = decoded.split(|&b| b == 0).collect();
        if parts.len() < 2 {
            return Err(XmppError::auth_failed("Invalid SASL PLAIN format"));
        }

        // For PLAIN: authzid \0 authcid \0 password
        // We use: \0 jid \0 token (authzid empty)
        let (jid_str, token) = if parts.len() == 3 {
            (
                String::from_utf8_lossy(parts[1]).to_string(),
                String::from_utf8_lossy(parts[2]).to_string(),
            )
        } else {
            (
                String::from_utf8_lossy(parts[0]).to_string(),
                String::from_utf8_lossy(parts[1]).to_string(),
            )
        };

        let jid: BareJid = jid_str
            .parse()
            .map_err(|e| XmppError::auth_failed(format!("Invalid JID: {}", e)))?;

        Ok((jid, token))
    }

    /// Send stream features for resource binding.
    #[instrument(skip(self), name = "xmpp.stream.send_features_bind")]
    pub async fn send_features_bind(&mut self) -> Result<(), XmppError> {
        let features = "\
            <stream:features>\
                <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/>\
                <session xmlns='urn:ietf:params:xml:ns:xmpp-session'>\
                    <optional/>\
                </session>\
            </stream:features>";

        self.write_all(features.as_bytes()).await?;

        debug!("Sent bind features");
        Ok(())
    }

    /// Handle resource binding.
    #[instrument(skip(self), name = "xmpp.stream.bind")]
    pub async fn handle_bind(&mut self, bare_jid: &BareJid) -> Result<FullJid, XmppError> {
        let mut buf = [0u8; 4096];
        let n = self.read(&mut buf).await?;

        if n == 0 {
            return Err(XmppError::stream("Connection closed during bind"));
        }

        let data = String::from_utf8_lossy(&buf[..n]);
        debug!(data = %data, "Received bind request");

        // Extract requested resource or generate one
        let resource = if data.contains("<resource>") {
            // Parse resource from request
            let start = data.find("<resource>").unwrap() + 10;
            let end = data.find("</resource>").unwrap_or(start);
            data[start..end].to_string()
        } else {
            // Generate random resource
            format!("waddle-{}", &uuid::Uuid::new_v4().to_string()[..8])
        };

        let full_jid = bare_jid.with_resource_str(&resource)
            .map_err(|e| XmppError::stream(format!("Invalid resource: {}", e)))?;

        // Send bind result
        // Extract the IQ id from the request
        let id = if let Some(start) = data.find("id='") {
            let start = start + 4;
            let end = data[start..].find('\'').map(|i| start + i).unwrap_or(start);
            &data[start..end]
        } else {
            "bind_1"
        };

        let result = format!(
            "<iq type='result' id='{}'>\
                <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
                    <jid>{}</jid>\
                </bind>\
            </iq>",
            id, full_jid
        );

        self.write_all(result.as_bytes()).await?;

        debug!(jid = %full_jid, "Resource bound");
        Ok(full_jid)
    }

    /// Read the next stanza from the stream.
    #[instrument(skip(self), name = "xmpp.stanza.read")]
    pub async fn read_stanza(&mut self) -> Result<Option<Stanza>, XmppError> {
        let mut buf = [0u8; 8192];
        let n = self.read(&mut buf).await?;

        if n == 0 {
            return Ok(None); // Connection closed
        }

        let data = String::from_utf8_lossy(&buf[..n]);
        debug!(data = %data, "Received stanza data");

        // Check for stream close
        if data.contains("</stream:stream>") {
            return Ok(None);
        }

        // TODO: Proper XML parsing with xmpp-parsers
        // For now, determine stanza type by tag
        if data.contains("<message") {
            // Parse message stanza
            // TODO: Proper XML parsing with xmpp-parsers
            Ok(Some(Stanza::Message(xmpp_parsers::message::Message::new(None))))
        } else if data.contains("<presence") {
            // TODO: Proper XML parsing with xmpp-parsers
            Ok(Some(Stanza::Presence(xmpp_parsers::presence::Presence::new(
                xmpp_parsers::presence::Type::None,
            ))))
        } else if data.contains("<iq") {
            // TODO: Proper XML parsing with xmpp-parsers
            // For now, return a placeholder IQ using ping
            Ok(Some(Stanza::Iq(xmpp_parsers::iq::Iq::from_get(
                "placeholder",
                xmpp_parsers::ping::Ping,
            ))))
        } else {
            debug!("Unknown stanza type, ignoring");
            // Return None and let the caller retry
            Ok(None)
        }
    }
}
