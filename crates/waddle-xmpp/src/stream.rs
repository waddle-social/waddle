//! XML stream handling for XMPP connections.

use base64::prelude::*;
use jid::{BareJid, FullJid};
use minidom::Element;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, instrument};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

use crate::auth::{parse_oauthbearer, OAuthBearerResult, SaslMechanism, ScramServer};
use crate::connection::Stanza;
use crate::parser::{element_to_string, ns, ParsedStanza, StreamHeader, XmlParser};
use crate::XmppError;

/// Result of SASL authentication.
#[derive(Debug)]
pub enum SaslAuthResult {
    /// PLAIN mechanism: JID and token extracted from credentials
    Plain {
        jid: BareJid,
        token: String,
    },
    /// OAUTHBEARER mechanism: token extracted from bearer credentials
    OAuthBearer {
        token: String,
        authzid: Option<String>,
    },
    /// OAUTHBEARER discovery request: client needs OAuth metadata
    OAuthBearerDiscovery,
    /// SCRAM-SHA-256 mechanism: client-first-message received, need password lookup
    /// The server should look up the stored keys for the username, then call
    /// `continue_scram_auth` with the keys to complete the exchange.
    ScramSha256Challenge {
        /// The username from client-first-message
        username: String,
        /// The server-first-message to send to the client (base64 encoded)
        server_first_message_b64: String,
        /// The SCRAM server state for continuing the exchange
        scram_server: ScramServer,
    },
    /// SCRAM-SHA-256 complete: authentication verified
    ScramSha256Complete {
        /// The authenticated username
        username: String,
    },
}

/// XMPP stream handler.
///
/// Manages the XML stream lifecycle including STARTTLS upgrade,
/// SASL authentication, and stanza reading/writing.
pub struct XmppStream {
    /// The underlying stream (either TCP or TLS)
    inner: StreamInner,
    /// Incremental XML parser
    parser: XmlParser,
    /// Server domain
    domain: String,
    /// Current stream ID
    stream_id: String,
    /// Parsed client stream header
    client_header: Option<StreamHeader>,
}

#[derive(Default)]
enum StreamInner {
    #[default]
    None,
    Tcp(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl XmppStream {
    /// Create a new XMPP stream from a TCP connection.
    pub fn new(stream: TcpStream, domain: String) -> Self {
        Self {
            inner: StreamInner::Tcp(stream),
            parser: XmlParser::new(),
            domain,
            stream_id: uuid::Uuid::new_v4().to_string(),
            client_header: None,
        }
    }

    /// Get the parsed client stream header.
    pub fn client_header(&self) -> Option<&StreamHeader> {
        self.client_header.as_ref()
    }

    /// Get the current stream ID.
    pub fn stream_id(&self) -> &str {
        &self.stream_id
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

    /// Flush the write buffer.
    async fn flush(&mut self) -> Result<(), XmppError> {
        match &mut self.inner {
            StreamInner::None => Err(XmppError::internal("Stream not initialized")),
            StreamInner::Tcp(s) => Ok(s.flush().await?),
            StreamInner::Tls(s) => Ok(s.flush().await?),
        }
    }

    /// Read data into the parser buffer until we have a complete stream header.
    #[instrument(skip(self), name = "xmpp.stream.read_header")]
    pub async fn read_stream_header(&mut self) -> Result<StreamHeader, XmppError> {
        // Reset parser for new stream
        self.parser.reset();
        self.stream_id = uuid::Uuid::new_v4().to_string();

        let mut buf = [0u8; 4096];

        // Read until we have a complete stream header
        loop {
            let n = self.read(&mut buf).await?;

            if n == 0 {
                return Err(XmppError::stream("Connection closed during header"));
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_stream_header() {
                break;
            }
        }

        let header = self.parser.take_stream_header()?;
        header.validate()?;

        debug!(
            to = ?header.to,
            from = ?header.from,
            version = ?header.version,
            "Received stream header"
        );

        self.client_header = Some(header.clone());

        // Send our stream header response
        self.send_stream_header().await?;

        Ok(header)
    }

    /// Send the server's stream header.
    async fn send_stream_header(&mut self) -> Result<(), XmppError> {
        let response = format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' \
            xmlns:stream='http://etherx.jabber.org/streams' \
            id='{}' from='{}' version='1.0'>",
            self.stream_id, self.domain
        );

        self.write_all(response.as_bytes()).await?;
        self.flush().await?;

        debug!(stream_id = %self.stream_id, "Sent stream header");
        Ok(())
    }

    /// Send stream features advertising STARTTLS.
    #[instrument(skip(self), name = "xmpp.stream.send_features_starttls")]
    pub async fn send_features_starttls(&mut self) -> Result<(), XmppError> {
        let features = format!(
            "<stream:features>\
                <starttls xmlns='{}'>\
                    <required/>\
                </starttls>\
            </stream:features>",
            ns::TLS
        );

        self.write_all(features.as_bytes()).await?;
        self.flush().await?;

        debug!("Sent STARTTLS features");
        Ok(())
    }

    /// Handle STARTTLS upgrade.
    #[instrument(skip(self, tls_acceptor), name = "xmpp.stream.starttls")]
    pub async fn handle_starttls(&mut self, tls_acceptor: TlsAcceptor) -> Result<(), XmppError> {
        // Read until we get a starttls request
        let mut buf = [0u8; 1024];

        loop {
            let n = match &mut self.inner {
                StreamInner::None => return Err(XmppError::internal("Stream not initialized")),
                StreamInner::Tcp(s) => s.read(&mut buf).await?,
                StreamInner::Tls(_) => return Err(XmppError::stream("Already using TLS")),
            };

            if n == 0 {
                return Err(XmppError::stream("Connection closed during STARTTLS"));
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_complete_stanza() {
                if let Some(ParsedStanza::StartTls) = self.parser.next_stanza()? {
                    break;
                }
            }
        }

        debug!("Received STARTTLS request");

        // Send proceed
        let proceed = format!("<proceed xmlns='{}'/>", ns::TLS);
        match &mut self.inner {
            StreamInner::None => return Err(XmppError::internal("Stream not initialized")),
            StreamInner::Tcp(s) => {
                s.write_all(proceed.as_bytes()).await?;
                s.flush().await?;
            }
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

        self.inner = StreamInner::Tls(Box::new(tls_stream));
        self.parser.reset();

        debug!("TLS upgrade complete");

        Ok(())
    }

    /// Send stream features advertising SASL mechanisms.
    ///
    /// Advertises SCRAM-SHA-256, PLAIN, and OAUTHBEARER mechanisms.
    /// SCRAM-SHA-256 is listed first as it's the most secure.
    /// OAUTHBEARER enables standard XMPP clients to use OAuth authentication.
    /// Also advertises ISR (XEP-0397) for instant stream resumption.
    #[instrument(skip(self), name = "xmpp.stream.send_features_sasl")]
    pub async fn send_features_sasl(&mut self) -> Result<(), XmppError> {
        let features = format!(
            "<stream:features>\
                <mechanisms xmlns='{}'>\
                    <mechanism>SCRAM-SHA-256</mechanism>\
                    <mechanism>PLAIN</mechanism>\
                    <mechanism>OAUTHBEARER</mechanism>\
                </mechanisms>\
                <isr xmlns='{}'/>\
            </stream:features>",
            ns::SASL, ns::ISR
        );

        self.write_all(features.as_bytes()).await?;
        self.flush().await?;

        debug!("Sent SASL features (SCRAM-SHA-256, PLAIN, OAUTHBEARER) with ISR");
        Ok(())
    }

    /// Handle SASL authentication.
    ///
    /// Returns the authentication result which can be:
    /// - PLAIN credentials (JID + token)
    /// - OAUTHBEARER credentials (token + optional authzid)
    /// - OAUTHBEARER discovery request (client needs OAuth metadata)
    /// - SCRAM-SHA-256 challenge (need to continue with `continue_scram_auth`)
    ///
    /// Note: This method does NOT send the SASL success response.
    /// The caller should validate credentials and then call either:
    /// - `send_sasl_success()` for a basic success response
    /// - `send_sasl_success_with_isr()` to include an ISR resumption token (XEP-0397)
    ///
    /// For OAUTHBEARER discovery, the caller should use send_oauthbearer_discovery()
    /// and then call this method again after the client completes OAuth.
    ///
    /// For SCRAM-SHA-256, the caller should:
    /// 1. Look up the stored keys for the username
    /// 2. Send the challenge using `send_scram_challenge()`
    /// 3. Call `continue_scram_auth()` with the stored keys
    #[instrument(skip(self), name = "xmpp.stream.authenticate")]
    pub async fn handle_sasl_auth(&mut self) -> Result<SaslAuthResult, XmppError> {
        let mut buf = [0u8; 4096];

        loop {
            let n = self.read(&mut buf).await?;

            if n == 0 {
                return Err(XmppError::stream("Connection closed during SASL"));
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_complete_stanza() {
                if let Some(ParsedStanza::SaslAuth { mechanism, data }) =
                    self.parser.next_stanza()?
                {
                    debug!(mechanism = %mechanism, "Received SASL auth");

                    let mech = SaslMechanism::from_str(&mechanism).ok_or_else(|| {
                        XmppError::auth_failed(format!("Unsupported mechanism: {}", mechanism))
                    })?;

                    match mech {
                        SaslMechanism::Plain => {
                            let (jid, token) = self.parse_sasl_plain(&data)?;
                            return Ok(SaslAuthResult::Plain { jid, token });
                        }
                        SaslMechanism::OAuthBearer => {
                            let result = self.parse_sasl_oauthbearer(&data)?;

                            match result {
                                OAuthBearerResult::DiscoveryRequest => {
                                    // Don't send success - caller should send discovery response
                                    debug!("OAUTHBEARER discovery request received");
                                    return Ok(SaslAuthResult::OAuthBearerDiscovery);
                                }
                                OAuthBearerResult::Credentials(creds) => {
                                    return Ok(SaslAuthResult::OAuthBearer {
                                        token: creds.token,
                                        authzid: creds.authzid,
                                    });
                                }
                            }
                        }
                        SaslMechanism::ScramSha256 => {
                            return self.handle_scram_client_first(&data);
                        }
                    }
                }
            }
        }
    }

    /// Handle SCRAM-SHA-256 client-first-message.
    ///
    /// This processes the initial SCRAM message and returns a challenge.
    /// The caller should look up the user's stored keys and call `continue_scram_auth`.
    fn handle_scram_client_first(&mut self, data: &str) -> Result<SaslAuthResult, XmppError> {
        // Decode base64
        let decoded = BASE64_STANDARD
            .decode(data.trim())
            .map_err(|e| XmppError::auth_failed(format!("Invalid base64: {}", e)))?;

        let client_first = String::from_utf8(decoded)
            .map_err(|e| XmppError::auth_failed(format!("Invalid UTF-8: {}", e)))?;

        debug!(client_first = %client_first, "SCRAM client-first-message");

        // Create SCRAM server and process client-first
        let mut scram_server = ScramServer::new();
        let server_first = scram_server.process_client_first(&client_first)?;

        // Base64 encode the server-first-message for the challenge
        let server_first_message_b64 = BASE64_STANDARD.encode(server_first.message.as_bytes());

        debug!(
            username = %server_first.username,
            "SCRAM-SHA-256 challenge generated"
        );

        Ok(SaslAuthResult::ScramSha256Challenge {
            username: server_first.username,
            server_first_message_b64,
            scram_server,
        })
    }

    /// Send SCRAM challenge (server-first-message).
    ///
    /// Call this after receiving `SaslAuthResult::ScramSha256Challenge` and looking up
    /// the user's stored keys.
    pub async fn send_scram_challenge(&mut self, server_first_message_b64: &str) -> Result<(), XmppError> {
        let challenge = format!(
            "<challenge xmlns='{}'>{}</challenge>",
            ns::SASL, server_first_message_b64
        );
        self.write_all(challenge.as_bytes()).await?;
        self.flush().await?;
        debug!("Sent SCRAM challenge");
        Ok(())
    }

    /// Continue SCRAM-SHA-256 authentication after sending challenge.
    ///
    /// This waits for the client-final-message and verifies the proof.
    ///
    /// # Arguments
    /// * `scram_server` - The ScramServer state from the challenge phase
    /// * `stored_key` - The user's StoredKey (from database)
    /// * `server_key` - The user's ServerKey (from database)
    ///
    /// # Returns
    /// * `SaslAuthResult::ScramSha256Complete` on success
    pub async fn continue_scram_auth(
        &mut self,
        mut scram_server: ScramServer,
        stored_key: &[u8],
        server_key: &[u8],
    ) -> Result<SaslAuthResult, XmppError> {
        let mut buf = [0u8; 4096];

        loop {
            let n = self.read(&mut buf).await?;

            if n == 0 {
                return Err(XmppError::stream("Connection closed during SCRAM"));
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_complete_stanza() {
                if let Some(ParsedStanza::SaslResponse { data }) = self.parser.next_stanza()? {
                    debug!("Received SCRAM client-final-message");

                    // Decode base64
                    let decoded = BASE64_STANDARD
                        .decode(data.trim())
                        .map_err(|e| XmppError::auth_failed(format!("Invalid base64: {}", e)))?;

                    let client_final = String::from_utf8(decoded)
                        .map_err(|e| XmppError::auth_failed(format!("Invalid UTF-8: {}", e)))?;

                    debug!(client_final = %client_final, "SCRAM client-final-message");

                    // Verify the client proof
                    let server_final = scram_server.process_client_final(
                        &client_final,
                        stored_key,
                        server_key,
                    )?;

                    // Send success with server signature
                    let server_signature_b64 = BASE64_STANDARD.encode(server_final.message.as_bytes());
                    let success = format!(
                        "<success xmlns='{}'>{}</success>",
                        ns::SASL, server_signature_b64
                    );
                    self.write_all(success.as_bytes()).await?;
                    self.flush().await?;

                    debug!("SCRAM-SHA-256 authentication successful");

                    return Ok(SaslAuthResult::ScramSha256Complete {
                        username: scram_server.username().to_string(),
                    });
                }
            }
        }
    }

    /// Handle SCRAM-SHA-256 authentication with custom salt and iterations.
    ///
    /// This is useful when the user already has stored SCRAM credentials with
    /// a specific salt and iteration count.
    ///
    /// # Arguments
    /// * `data` - The base64-encoded client-first-message
    /// * `salt_b64` - The user's salt (base64 encoded)
    /// * `iterations` - The iteration count
    ///
    /// # Returns
    /// * `SaslAuthResult::ScramSha256Challenge` with the configured ScramServer
    pub fn handle_scram_client_first_with_params(
        &mut self,
        data: &str,
        salt_b64: String,
        iterations: u32,
    ) -> Result<SaslAuthResult, XmppError> {
        // Decode base64
        let decoded = BASE64_STANDARD
            .decode(data.trim())
            .map_err(|e| XmppError::auth_failed(format!("Invalid base64: {}", e)))?;

        let client_first = String::from_utf8(decoded)
            .map_err(|e| XmppError::auth_failed(format!("Invalid UTF-8: {}", e)))?;

        debug!(client_first = %client_first, "SCRAM client-first-message (with params)");

        // Create SCRAM server with user's stored parameters
        let mut scram_server = ScramServer::with_salt_b64(salt_b64, iterations);
        let server_first = scram_server.process_client_first(&client_first)?;

        // Base64 encode the server-first-message for the challenge
        let server_first_message_b64 = BASE64_STANDARD.encode(server_first.message.as_bytes());

        debug!(
            username = %server_first.username,
            iterations = iterations,
            "SCRAM-SHA-256 challenge generated (with params)"
        );

        Ok(SaslAuthResult::ScramSha256Challenge {
            username: server_first.username,
            server_first_message_b64,
            scram_server,
        })
    }

    /// Send SASL success response.
    ///
    /// Call this after validating credentials from `handle_sasl_auth()`.
    pub async fn send_sasl_success(&mut self) -> Result<(), XmppError> {
        let success = format!("<success xmlns='{}'/>", ns::SASL);
        self.write_all(success.as_bytes()).await?;
        self.flush().await?;
        debug!("Sent SASL success");
        Ok(())
    }

    /// Send SASL success response with ISR resumption token (XEP-0397).
    ///
    /// Call this after validating credentials from `handle_sasl_auth()`.
    /// The ISR token allows the client to resume the stream without re-authenticating.
    ///
    /// Response format per XEP-0397:
    /// ```xml
    /// <success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>
    ///   <token xmlns='urn:xmpp:isr:0' expiry='ISO8601'>TOKEN</token>
    /// </success>
    /// ```
    pub async fn send_sasl_success_with_isr(&mut self, isr_token_xml: &str) -> Result<(), XmppError> {
        let success = format!(
            "<success xmlns='{}'>{}</success>",
            ns::SASL, isr_token_xml
        );
        self.write_all(success.as_bytes()).await?;
        self.flush().await?;
        debug!("Sent SASL success with ISR token");
        Ok(())
    }

    /// Send SASL failure response.
    pub async fn send_sasl_failure(&mut self, condition: &str) -> Result<(), XmppError> {
        let failure = format!(
            "<failure xmlns='{}'><{}/></failure>",
            ns::SASL, condition
        );
        self.write_all(failure.as_bytes()).await?;
        self.flush().await?;
        Ok(())
    }

    /// Send OAUTHBEARER discovery response per XEP-0493 §3.2.
    ///
    /// When a client sends an empty OAUTHBEARER request, we respond with
    /// the OAuth authorization server discovery URL so the client can
    /// complete the OAuth flow.
    ///
    /// Response format per XEP-0493:
    /// ```xml
    /// <failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>
    ///   <not-authorized/>
    ///   <openid-configuration>https://example.com/.well-known/oauth-authorization-server</openid-configuration>
    /// </failure>
    /// ```
    pub async fn send_oauthbearer_discovery(&mut self, discovery_url: &str) -> Result<(), XmppError> {
        let response = format!(
            "<failure xmlns='{}'>\
                <not-authorized/>\
                <openid-configuration>{}</openid-configuration>\
            </failure>",
            ns::SASL, discovery_url
        );
        self.write_all(response.as_bytes()).await?;
        self.flush().await?;

        debug!(discovery_url = %discovery_url, "Sent OAUTHBEARER discovery response");
        Ok(())
    }

    /// Parse SASL PLAIN authentication data.
    fn parse_sasl_plain(&self, data: &str) -> Result<(BareJid, String), XmppError> {
        // Decode base64
        let decoded = BASE64_STANDARD
            .decode(data.trim())
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

    /// Parse SASL OAUTHBEARER authentication data (RFC 7628).
    fn parse_sasl_oauthbearer(&self, data: &str) -> Result<OAuthBearerResult, XmppError> {
        // Decode base64
        let decoded = BASE64_STANDARD
            .decode(data.trim())
            .map_err(|e| XmppError::auth_failed(format!("Invalid base64: {}", e)))?;

        // Use the auth module parser
        parse_oauthbearer(&decoded)
    }

    /// Send stream features for resource binding.
    #[instrument(skip(self), name = "xmpp.stream.send_features_bind")]
    pub async fn send_features_bind(&mut self) -> Result<(), XmppError> {
        // XEP-0198: Stream Management is advertised alongside bind
        // XEP-0397: ISR is also advertised for instant stream resumption
        let features = format!(
            "<stream:features>\
                <bind xmlns='{}'/>\
                <session xmlns='{}'>\
                    <optional/>\
                </session>\
                <sm xmlns='{}'/>\
                <isr xmlns='{}'/>\
            </stream:features>",
            ns::BIND, ns::SESSION, ns::SM, ns::ISR
        );

        self.write_all(features.as_bytes()).await?;
        self.flush().await?;

        debug!("Sent bind features (with Stream Management and ISR)");
        Ok(())
    }

    /// Send stream features with only Stream Management (after bind, if SM was enabled).
    #[instrument(skip(self), name = "xmpp.stream.send_features_sm_only")]
    pub async fn send_features_sm(&mut self) -> Result<(), XmppError> {
        let features = format!(
            "<stream:features>\
                <sm xmlns='{}'/>\
            </stream:features>",
            ns::SM
        );

        self.write_all(features.as_bytes()).await?;
        self.flush().await?;

        debug!("Sent Stream Management features");
        Ok(())
    }

    /// Send XEP-0198 Stream Management enabled response.
    pub async fn send_sm_enabled(&mut self, stream_id: &str, resume: bool, max_seconds: Option<u32>) -> Result<(), XmppError> {
        let mut attrs = format!("id='{}'", stream_id);
        if resume {
            attrs.push_str(" resume='true'");
        }
        if let Some(max) = max_seconds {
            attrs.push_str(&format!(" max='{}'", max));
        }

        let response = format!("<enabled xmlns='{}' {}/>", ns::SM, attrs);
        self.write_all(response.as_bytes()).await?;
        self.flush().await?;

        debug!(stream_id = %stream_id, resume = resume, "Sent SM enabled");
        Ok(())
    }

    /// Send XEP-0198 Stream Management acknowledgment.
    pub async fn send_sm_ack(&mut self, h: u32) -> Result<(), XmppError> {
        let response = format!("<a xmlns='{}' h='{}'/>", ns::SM, h);
        self.write_all(response.as_bytes()).await?;
        self.flush().await?;

        debug!(h = h, "Sent SM ack");
        Ok(())
    }

    /// Send XEP-0198 Stream Management request.
    pub async fn send_sm_request(&mut self) -> Result<(), XmppError> {
        let request = format!("<r xmlns='{}'/>", ns::SM);
        self.write_all(request.as_bytes()).await?;
        self.flush().await?;

        debug!("Sent SM request");
        Ok(())
    }

    /// Send XEP-0198 Stream Management failed response.
    pub async fn send_sm_failed(&mut self, condition: Option<&str>, h: Option<u32>) -> Result<(), XmppError> {
        let h_attr = h.map(|h| format!(" h='{}'", h)).unwrap_or_default();

        let response = if let Some(cond) = condition {
            format!(
                "<failed xmlns='{}'{}><{} xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></failed>",
                ns::SM, h_attr, cond
            )
        } else {
            format!("<failed xmlns='{}'{}/>", ns::SM, h_attr)
        };

        self.write_all(response.as_bytes()).await?;
        self.flush().await?;

        debug!(condition = ?condition, h = ?h, "Sent SM failed");
        Ok(())
    }

    /// Send XEP-0198 Stream Management resumed response.
    ///
    /// Called when a stream is successfully resumed using a previous stream ID.
    pub async fn send_sm_resumed(&mut self, previd: &str, h: u32) -> Result<(), XmppError> {
        let response = format!(
            "<resumed xmlns='{}' previd='{}' h='{}'/>",
            ns::SM, previd, h
        );

        self.write_all(response.as_bytes()).await?;
        self.flush().await?;

        debug!(previd = %previd, h = h, "Sent SM resumed");
        Ok(())
    }

    /// Send XEP-0198 Stream Management resumed response with ISR token (XEP-0397).
    ///
    /// Called when a stream is successfully resumed using an ISR token.
    /// The response includes a new ISR token for future resumption.
    ///
    /// Response format per XEP-0397:
    /// ```xml
    /// <resumed xmlns='urn:xmpp:sm:3' previd='...' h='N'>
    ///   <token xmlns='urn:xmpp:isr:0' expiry='ISO8601'>NEW_TOKEN</token>
    /// </resumed>
    /// ```
    pub async fn send_sm_resumed_with_isr(&mut self, previd: &str, h: u32, isr_token_xml: &str) -> Result<(), XmppError> {
        let response = format!(
            "<resumed xmlns='{}' previd='{}' h='{}'>{}</resumed>",
            ns::SM, previd, h, isr_token_xml
        );

        self.write_all(response.as_bytes()).await?;
        self.flush().await?;

        debug!(previd = %previd, h = h, "Sent SM resumed with ISR token");
        Ok(())
    }

    /// Handle resource binding.
    #[instrument(skip(self), name = "xmpp.stream.bind")]
    pub async fn handle_bind(&mut self, bare_jid: &BareJid) -> Result<FullJid, XmppError> {
        let mut buf = [0u8; 4096];

        loop {
            let n = self.read(&mut buf).await?;

            if n == 0 {
                return Err(XmppError::stream("Connection closed during bind"));
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_complete_stanza() {
                if let Some(ParsedStanza::Iq(element)) = self.parser.next_stanza()? {
                    debug!("Received bind request");

                    // Extract IQ attributes
                    let id = element.attr("id").unwrap_or("bind_1").to_string();
                    let iq_type = element.attr("type").unwrap_or("");

                    if iq_type != "set" {
                        return Err(XmppError::stream("Bind must be an IQ set"));
                    }

                    // Look for bind element and optional resource
                    let resource = element
                        .get_child("bind", ns::BIND)
                        .and_then(|bind| bind.get_child("resource", ns::BIND))
                        .map(|r| r.text())
                        .unwrap_or_else(|| {
                            format!("waddle-{}", &uuid::Uuid::new_v4().to_string()[..8])
                        });

                    let full_jid = bare_jid
                        .with_resource_str(&resource)
                        .map_err(|e| XmppError::stream(format!("Invalid resource: {}", e)))?;

                    // Send bind result
                    let result = format!(
                        "<iq type='result' id='{}'>\
                            <bind xmlns='{}'>\
                                <jid>{}</jid>\
                            </bind>\
                        </iq>",
                        id, ns::BIND, full_jid
                    );

                    self.write_all(result.as_bytes()).await?;
                    self.flush().await?;

                    debug!(jid = %full_jid, "Resource bound");
                    return Ok(full_jid);
                }
            }
        }
    }

    /// Read the next stanza from the stream.
    #[instrument(skip(self), name = "xmpp.stanza.read")]
    pub async fn read_stanza(&mut self) -> Result<Option<Stanza>, XmppError> {
        let mut buf = [0u8; 8192];

        loop {
            // First check if we already have a complete stanza buffered
            if self.parser.has_complete_stanza() {
                return self.process_parsed_stanza();
            }

            // Read more data
            let n = self.read(&mut buf).await?;

            if n == 0 {
                return Ok(None); // Connection closed
            }

            self.parser.feed(&buf[..n]);

            // Check again
            if self.parser.has_complete_stanza() {
                return self.process_parsed_stanza();
            }
        }
    }

    /// Process a parsed stanza from the parser.
    fn process_parsed_stanza(&mut self) -> Result<Option<Stanza>, XmppError> {
        match self.parser.next_stanza()? {
            Some(ParsedStanza::StreamEnd) => Ok(None),
            Some(ParsedStanza::Message(element)) => {
                let msg = element_to_message(element)?;
                Ok(Some(Stanza::Message(msg)))
            }
            Some(ParsedStanza::Presence(element)) => {
                let pres = element_to_presence(element)?;
                Ok(Some(Stanza::Presence(pres)))
            }
            Some(ParsedStanza::Iq(element)) => {
                let iq = element_to_iq(element)?;
                Ok(Some(Stanza::Iq(iq)))
            }
            Some(_) => {
                // Other stanza types (shouldn't happen at this point)
                debug!("Unexpected stanza type in established session");
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Read the next raw parsed stanza from the stream (includes SM stanzas).
    ///
    /// This is used by the connection actor to handle both regular stanzas and
    /// XEP-0198 Stream Management stanzas.
    #[instrument(skip(self), name = "xmpp.stanza.read_raw")]
    pub async fn read_parsed_stanza(&mut self) -> Result<Option<ParsedStanza>, XmppError> {
        let mut buf = [0u8; 8192];

        loop {
            // First check if we already have a complete stanza buffered
            if self.parser.has_complete_stanza() {
                return Ok(self.parser.next_stanza()?);
            }

            // Read more data
            let n = self.read(&mut buf).await?;

            if n == 0 {
                return Ok(None); // Connection closed
            }

            self.parser.feed(&buf[..n]);

            // Check again
            if self.parser.has_complete_stanza() {
                return Ok(self.parser.next_stanza()?);
            }
        }
    }

    /// Write a stanza to the stream.
    pub async fn write_stanza(&mut self, stanza: &Stanza) -> Result<(), XmppError> {
        let xml = stanza_to_xml(stanza)?;
        self.write_all(xml.as_bytes()).await?;
        self.flush().await?;
        Ok(())
    }

    /// Write raw XML to the stream.
    pub async fn write_raw(&mut self, xml: &str) -> Result<(), XmppError> {
        self.write_all(xml.as_bytes()).await?;
        self.flush().await?;
        Ok(())
    }

    /// Close the stream gracefully.
    pub async fn close(&mut self) -> Result<(), XmppError> {
        self.write_all(b"</stream:stream>").await?;
        self.flush().await?;
        Ok(())
    }
}

/// Convert a minidom Element to an xmpp_parsers Message.
fn element_to_message(element: Element) -> Result<Message, XmppError> {
    Message::try_from(element).map_err(|e| XmppError::xml_parse(format!("Invalid message: {:?}", e)))
}

/// Convert a minidom Element to an xmpp_parsers Presence.
fn element_to_presence(element: Element) -> Result<Presence, XmppError> {
    Presence::try_from(element)
        .map_err(|e| XmppError::xml_parse(format!("Invalid presence: {:?}", e)))
}

/// Convert a minidom Element to an xmpp_parsers Iq.
fn element_to_iq(element: Element) -> Result<Iq, XmppError> {
    Iq::try_from(element).map_err(|e| XmppError::xml_parse(format!("Invalid iq: {:?}", e)))
}

/// Convert a Stanza to XML string.
fn stanza_to_xml(stanza: &Stanza) -> Result<String, XmppError> {
    match stanza {
        Stanza::Message(msg) => {
            let element: Element = msg.clone().into();
            element_to_string(&element)
        }
        Stanza::Presence(pres) => {
            let element: Element = pres.clone().into();
            element_to_string(&element)
        }
        Stanza::Iq(iq) => {
            let element: Element = iq.clone().into();
            element_to_string(&element)
        }
    }
}
