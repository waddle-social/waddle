//! Process startup of the S2S service: whether it runs, and the snapshot it
//! runs with.

use std::sync::Arc;

use thiserror::Error;
use waddle_xmpp::identity::CanonicalDomain;

use crate::s2s::config::{ConfigError, S2sActivation, S2sConfig};
use crate::s2s::verify::TlsSettings;
use crate::server::XmppConfig;

/// The `LISTEN_FD_NAMES` name of an inherited S2S listener.
pub const S2S_LISTENER_NAME: &str = "s2s";

/// Why S2S could not start. The process does not start either: an operator
/// who enabled S2S gets it or an error, never a silent fallback.
#[derive(Debug, Error)]
pub enum StartupError {
    #[error("S2S configuration: {0}")]
    Config(#[from] ConfigError),
    #[error("S2S local domain {index} is not the XMPP domain, the MUC domain or the Spaces domain this process serves")]
    UnservedLocalDomain { index: usize },
}

/// Load and validate the S2S snapshot when the environment enables S2S.
///
/// While S2S is off this returns `None` without reading any S2S file. Its
/// TLS configurations use the ring provider, pinned here rather than taken
/// from the process default, so the verifier's algorithm set and the TLS
/// stack's always agree.
pub async fn load_enabled_config(
    xmpp: &XmppConfig,
) -> Result<Option<Arc<S2sConfig>>, StartupError> {
    let path = match S2sActivation::from_env()? {
        S2sActivation::Disabled => return Ok(None),
        S2sActivation::Enabled(path) => path,
    };
    let settings = TlsSettings::new(Arc::new(rustls::crypto::ring::default_provider()));
    let config = S2sConfig::load(&path, settings).await?;
    check_local_domains(&config, xmpp)?;
    Ok(Some(Arc::new(config)))
}

/// Every S2S local domain must be a domain this process serves to remote
/// senders: the account domain, the MUC service or the Spaces service
/// (ADR-0020 D10).
fn check_local_domains(config: &S2sConfig, xmpp: &XmppConfig) -> Result<(), StartupError> {
    let served: Vec<CanonicalDomain> = [
        CanonicalDomain::prepare(&xmpp.domain).ok(),
        CanonicalDomain::from_domain(&xmpp.muc_domain).ok(),
        CanonicalDomain::from_domain(xmpp.spaces_jid.domain()).ok(),
    ]
    .into_iter()
    .flatten()
    .collect();
    match config
        .local_domains()
        .iter()
        .position(|domain| !served.contains(domain))
    {
        Some(index) => Err(StartupError::UnservedLocalDomain { index }),
        None => Ok(()),
    }
}
