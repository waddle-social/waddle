use crate::server::XmppConfig;
use futures::StreamExt;
use rustls_acme::caches::DirCache;
use rustls_acme::tower::TowerHttp01ChallengeService;
use rustls_acme::{AcmeConfig, UseChallenge};
use tracing::{info, warn};

#[derive(Clone)]
pub(crate) struct AcmeRuntime {
    pub(crate) http01_challenge_service: TowerHttp01ChallengeService,
}

pub(crate) fn start_acme_runtime(
    xmpp_config: &XmppConfig,
    stop_token: tokio_util::sync::CancellationToken,
) -> Option<AcmeRuntime> {
    if !xmpp_config.enabled || !xmpp_config.acme.enabled {
        return None;
    }

    if xmpp_config.domain == "localhost" {
        warn!(
            "ACME is enabled but XMPP domain is localhost; public DNS domain is required for Let's Encrypt"
        );
    }

    let mut acme_config = AcmeConfig::new([xmpp_config.domain.as_str()])
        .cache(DirCache::new(xmpp_config.acme.cache_dir.clone()))
        .directory_lets_encrypt(xmpp_config.acme.production)
        .challenge_type(UseChallenge::Http01);

    if let Some(email) = xmpp_config.acme.email.as_deref() {
        let contact = if email.starts_with("mailto:") {
            email.to_string()
        } else {
            format!("mailto:{email}")
        };
        acme_config = acme_config.contact_push(contact);
    } else {
        warn!("ACME is enabled without WADDLE_XMPP_ACME_EMAIL; proceeding without contact email");
    }

    let mut state = acme_config.state();
    let http01_challenge_service = state.http01_challenge_tower_service();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop_token.cancelled() => {
                    info!("ACME task stopped (shutdown token cancelled)");
                    break;
                }
                event = state.next() => {
                    match event {
                        Some(Ok(ok)) => info!(event = ?ok, "ACME event"),
                        Some(Err(err)) => warn!(error = %err, "ACME event failed"),
                        None => {
                            warn!("ACME stream ended unexpectedly");
                            break;
                        }
                    }
                }
            }
        }
    });

    info!(
        domain = %xmpp_config.domain,
        production = xmpp_config.acme.production,
        cache_dir = %xmpp_config.acme.cache_dir.display(),
        "ACME certificate management enabled (HTTP-01)"
    );

    Some(AcmeRuntime {
        http01_challenge_service,
    })
}
