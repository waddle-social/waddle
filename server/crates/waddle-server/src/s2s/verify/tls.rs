//! The S2S TLS profile: protocol versions and connection settings.
//!
//! Every connection gets its own rustls configuration, built here around
//! its [`PeerVerifier`]. The profile admits TLS 1.3 and TLS 1.2 (RFC 9325
//! §3.1.1) and prefers TLS 1.3; TLS 1.2 is limited to the provider's
//! forward-secret AEAD suites and requires the extended master secret
//! (RFC 7627). Session resumption, tickets, early data and certificate
//! compression are off, so every connection runs a full handshake and every
//! handshake verifies the peer's certificate chain and handshake signature
//! again.

use std::sync::Arc;

use rustls::client::Resumption;
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::UnixTime;
use rustls::server::NoServerSessionStorage;
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::time_provider::{DefaultTimeProvider, TimeProvider};
use rustls::version::{TLS12, TLS13};
use rustls::{ClientConfig, InconsistentKeys, ServerConfig, SupportedProtocolVersion};

use super::adapter::PeerVerifier;
use super::error::TlsConfigError;

/// The TLS versions of the S2S profile, most preferred first.
pub const TLS_PROTOCOL_VERSIONS: &[&SupportedProtocolVersion] = &[&TLS13, &TLS12];

/// The ALPN protocol of a direct-TLS S2S connection (XEP-0368 §6).
pub const DIRECT_TLS_ALPN: &[u8] = b"xmpp-server";

/// The crypto provider and clock shared by every S2S TLS configuration.
#[derive(Debug, Clone)]
pub struct TlsSettings {
    provider: Arc<CryptoProvider>,
    time: Arc<dyn TimeProvider>,
}

impl TlsSettings {
    /// Settings that use `provider` and the wall clock.
    pub fn new(provider: Arc<CryptoProvider>) -> Self {
        Self {
            provider,
            time: Arc::new(DefaultTimeProvider),
        }
    }

    /// Replace the clock that certificate and CRL validity is checked
    /// against.
    pub fn with_time_provider(mut self, time: Arc<dyn TimeProvider>) -> Self {
        self.time = time;
        self
    }

    /// The crypto provider, which also loads the local private keys.
    pub fn provider(&self) -> &Arc<CryptoProvider> {
        &self.provider
    }

    /// The signature algorithms the provider verifies. Certificate chains,
    /// handshake signatures and configured trust material all use this one
    /// set.
    pub fn algorithms(&self) -> WebPkiSupportedAlgorithms {
        self.provider.signature_verification_algorithms
    }

    /// The current time, or `None` when the clock cannot tell it; a caller
    /// that needs the time then fails closed.
    pub fn now(&self) -> Option<UnixTime> {
        self.time.current_time()
    }
}

/// How an initiating connection reaches TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutboundTransport {
    /// STARTTLS on an `_xmpp-server` target.
    StartTls,
    /// Direct TLS to an `_xmpps-server` target (XEP-0368), which sends the
    /// `xmpp-server` ALPN protocol.
    DirectTls,
}

/// The TLS configuration of one initiating connection.
///
/// `credentials` authenticate the local source domain and `verifier`
/// verifies the receiving server. Open the connection with
/// [`super::ReferenceIdentity::server_name`], which is also the Server Name
/// Indication it sends.
pub fn initiating_config(
    settings: &TlsSettings,
    credentials: Arc<CertifiedKey>,
    verifier: Arc<PeerVerifier>,
    transport: OutboundTransport,
) -> Result<ClientConfig, TlsConfigError> {
    check_credentials(&credentials)?;
    let mut config = ClientConfig::builder_with_details(
        Arc::clone(&settings.provider),
        Arc::clone(&settings.time),
    )
    .with_protocol_versions(TLS_PROTOCOL_VERSIONS)
    .map_err(TlsConfigError::Provider)?
    .dangerous()
    .with_custom_certificate_verifier(verifier)
    .with_client_cert_resolver(Arc::new(SingleCertAndKey::from(credentials)));
    config.resumption = Resumption::disabled();
    config.enable_early_data = false;
    config.enable_sni = true;
    config.require_ems = true;
    config.enable_secret_extraction = false;
    config.cert_compressors = Vec::new();
    config.cert_decompressors = Vec::new();
    config.alpn_protocols = match transport {
        OutboundTransport::StartTls => Vec::new(),
        OutboundTransport::DirectTls => vec![DIRECT_TLS_ALPN.to_vec()],
    };
    Ok(config)
}

/// The TLS configuration of one receiving connection.
///
/// `credentials` authenticate the local destination domain and `verifier`
/// verifies the initiating server, which must present a certificate.
pub fn receiving_config(
    settings: &TlsSettings,
    credentials: Arc<CertifiedKey>,
    verifier: Arc<PeerVerifier>,
) -> Result<ServerConfig, TlsConfigError> {
    check_credentials(&credentials)?;
    let mut config = ServerConfig::builder_with_details(
        Arc::clone(&settings.provider),
        Arc::clone(&settings.time),
    )
    .with_protocol_versions(TLS_PROTOCOL_VERSIONS)
    .map_err(TlsConfigError::Provider)?
    .with_client_cert_verifier(verifier)
    .with_cert_resolver(Arc::new(SingleCertAndKey::from(credentials)));
    // No session is stored and no ticket is issued, so neither TLS version
    // can resume; the builder's default ticketer never produces tickets and
    // is left in place.
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    config.max_early_data_size = 0;
    config.send_half_rtt_data = false;
    config.require_ems = true;
    config.enable_secret_extraction = false;
    config.cert_compressors = Vec::new();
    config.cert_decompressors = Vec::new();
    Ok(config)
}

/// Require that the private key belongs to the end-entity certificate. A
/// key that cannot report its public key is accepted, as rustls does.
fn check_credentials(credentials: &CertifiedKey) -> Result<(), TlsConfigError> {
    match credentials.keys_match() {
        Ok(()) | Err(rustls::Error::InconsistentKeys(InconsistentKeys::Unknown)) => Ok(()),
        Err(rustls::Error::InconsistentKeys(InconsistentKeys::KeyMismatch)) => {
            Err(TlsConfigError::KeyMismatch)
        }
        Err(error) => Err(TlsConfigError::Credentials(error)),
    }
}
