use std::collections::HashSet;

#[cfg(any(feature = "native", test))]
use sasl::client::Mechanism;
#[cfg(any(feature = "native", test))]
use sasl::client::mechanisms::{Plain, Scram};
#[cfg(any(feature = "native", test))]
use sasl::common::Credentials;
#[cfg(any(feature = "native", test))]
use sasl::common::scram::{Sha1, Sha256};

#[cfg(any(feature = "native", test))]
use crate::error::ConnectionError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedMechanism {
    ScramSha256,
    ScramSha1,
    Plain,
}

impl SelectedMechanism {
    pub fn name(&self) -> &'static str {
        match self {
            SelectedMechanism::ScramSha256 => "SCRAM-SHA-256",
            SelectedMechanism::ScramSha1 => "SCRAM-SHA-1",
            SelectedMechanism::Plain => "PLAIN",
        }
    }
}

impl std::fmt::Display for SelectedMechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

const MECHANISM_PREFERENCE: &[SelectedMechanism] = &[
    SelectedMechanism::ScramSha256,
    SelectedMechanism::ScramSha1,
    SelectedMechanism::Plain,
];

pub fn select_mechanism(server_mechanisms: &HashSet<String>) -> Option<SelectedMechanism> {
    MECHANISM_PREFERENCE
        .iter()
        .find(|m| server_mechanisms.contains(m.name()))
        .copied()
}

#[cfg(any(feature = "native", test))]
fn build_mechanism(
    selected: SelectedMechanism,
    credentials: &Credentials,
) -> Result<Box<dyn Mechanism + Send>, ConnectionError> {
    match selected {
        SelectedMechanism::ScramSha256 => Scram::<Sha256>::from_credentials(credentials.clone())
            .map(|m| Box::new(m) as Box<dyn Mechanism + Send>)
            .map_err(|e| {
                ConnectionError::AuthenticationFailed(format!(
                    "failed to initialize SCRAM-SHA-256: {e:?}"
                ))
            }),
        SelectedMechanism::ScramSha1 => Scram::<Sha1>::from_credentials(credentials.clone())
            .map(|m| Box::new(m) as Box<dyn Mechanism + Send>)
            .map_err(|e| {
                ConnectionError::AuthenticationFailed(format!(
                    "failed to initialize SCRAM-SHA-1: {e:?}"
                ))
            }),
        SelectedMechanism::Plain => Plain::from_credentials(credentials.clone())
            .map(|m| Box::new(m) as Box<dyn Mechanism + Send>)
            .map_err(|e| {
                ConnectionError::AuthenticationFailed(format!("failed to initialize PLAIN: {e:?}"))
            }),
    }
}

#[cfg(feature = "native")]
mod native {
    use std::collections::HashSet;
    use std::str::FromStr;

    use futures::StreamExt;
    use sasl::common::{ChannelBinding, Credentials};
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio_xmpp::Packet;
    use tokio_xmpp::parsers::sasl::{
        Auth, Challenge, Failure, Mechanism as SaslMechanism, Response, Success,
    };
    use tokio_xmpp::xmpp_stream::XMPPStream;
    use tracing::{debug, warn};

    use super::{build_mechanism, select_mechanism};
    use crate::error::ConnectionError;

    pub(crate) fn map_failure(failure: &Failure) -> ConnectionError {
        let condition = format!("{:?}", failure.defined_condition);
        let text = failure.texts.values().next().cloned().unwrap_or_default();

        if text.is_empty() {
            ConnectionError::AuthenticationFailed(condition)
        } else {
            ConnectionError::AuthenticationFailed(format!("{condition}: {text}"))
        }
    }

    pub async fn authenticate<S>(
        mut stream: XMPPStream<S>,
        username: &str,
        password: &str,
    ) -> Result<S, ConnectionError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let server_mechanisms: HashSet<String> = stream
            .stream_features
            .sasl_mechanisms()
            .map_err(|_| {
                ConnectionError::AuthenticationFailed(
                    "server did not advertise any SASL mechanisms".to_string(),
                )
            })?
            .collect();

        debug!(
            mechanisms = ?server_mechanisms,
            "server advertised SASL mechanisms"
        );

        let selected = select_mechanism(&server_mechanisms).ok_or_else(|| {
            ConnectionError::AuthenticationFailed(format!(
                "no supported SASL mechanism found; server offers: {}",
                server_mechanisms
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;

        debug!(mechanism = %selected, "selected SASL mechanism");

        let credentials = Credentials::default()
            .with_username(username)
            .with_password(password)
            .with_channel_binding(ChannelBinding::Unsupported);

        let mut mechanism = build_mechanism(selected, &credentials)?;
        let initial_data = mechanism.initial();

        let mechanism_name = SaslMechanism::from_str(mechanism.name()).map_err(|e| {
            ConnectionError::AuthenticationFailed(format!("invalid SASL mechanism name: {e}"))
        })?;

        stream
            .send_stanza(Auth {
                mechanism: mechanism_name,
                data: initial_data,
            })
            .await
            .map_err(|e| ConnectionError::StreamError(format!("failed to send SASL auth: {e}")))?;

        loop {
            match stream.next().await {
                Some(Ok(Packet::Stanza(stanza))) => {
                    if let Ok(challenge) = Challenge::try_from(stanza.clone()) {
                        let response_data = mechanism.response(&challenge.data).map_err(|e| {
                            ConnectionError::AuthenticationFailed(format!(
                                "SASL challenge-response failed: {e:?}"
                            ))
                        })?;

                        stream
                            .send_stanza(Response {
                                data: response_data,
                            })
                            .await
                            .map_err(|e| {
                                ConnectionError::StreamError(format!(
                                    "failed to send SASL response: {e}"
                                ))
                            })?;
                    } else if let Ok(success) = Success::try_from(stanza.clone()) {
                        if let Err(e) = mechanism.success(&success.data) {
                            warn!(error = ?e, "server signature verification failed");
                            return Err(ConnectionError::AuthenticationFailed(format!(
                                "server signature verification failed: {e:?}"
                            )));
                        }

                        debug!("SASL authentication succeeded");
                        return Ok(stream.into_inner());
                    } else if let Ok(failure) = Failure::try_from(stanza) {
                        debug!(condition = ?failure.defined_condition, "SASL authentication failed");
                        return Err(map_failure(&failure));
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    return Err(ConnectionError::StreamError(format!(
                        "stream error during SASL negotiation: {e}"
                    )));
                }
                None => {
                    return Err(ConnectionError::TransportError(
                        "connection closed during SASL negotiation".to_string(),
                    ));
                }
            }
        }
    }
}

#[cfg(feature = "native")]
pub use native::authenticate;

#[cfg(test)]
mod tests {
    use sasl::common::ChannelBinding;

    use super::*;

    #[test]
    fn selects_scram_sha256_when_available() {
        let server = HashSet::from([
            "PLAIN".to_string(),
            "SCRAM-SHA-1".to_string(),
            "SCRAM-SHA-256".to_string(),
        ]);
        assert_eq!(
            select_mechanism(&server),
            Some(SelectedMechanism::ScramSha256)
        );
    }

    #[test]
    fn falls_back_to_scram_sha1() {
        let server = HashSet::from(["PLAIN".to_string(), "SCRAM-SHA-1".to_string()]);
        assert_eq!(
            select_mechanism(&server),
            Some(SelectedMechanism::ScramSha1)
        );
    }

    #[test]
    fn falls_back_to_plain() {
        let server = HashSet::from(["PLAIN".to_string()]);
        assert_eq!(select_mechanism(&server), Some(SelectedMechanism::Plain));
    }

    #[test]
    fn returns_none_when_no_supported_mechanism() {
        let server = HashSet::from(["EXTERNAL".to_string(), "GSSAPI".to_string()]);
        assert_eq!(select_mechanism(&server), None);
    }

    #[test]
    fn returns_none_for_empty_mechanisms() {
        let server = HashSet::new();
        assert_eq!(select_mechanism(&server), None);
    }

    #[test]
    fn build_scram_sha256_succeeds() {
        let creds = Credentials::default()
            .with_username("alice")
            .with_password("secret")
            .with_channel_binding(ChannelBinding::Unsupported);
        let result = build_mechanism(SelectedMechanism::ScramSha256, &creds);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name(), "SCRAM-SHA-256");
    }

    #[test]
    fn build_scram_sha1_succeeds() {
        let creds = Credentials::default()
            .with_username("alice")
            .with_password("secret")
            .with_channel_binding(ChannelBinding::Unsupported);
        let result = build_mechanism(SelectedMechanism::ScramSha1, &creds);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name(), "SCRAM-SHA-1");
    }

    #[test]
    fn build_plain_succeeds() {
        let creds = Credentials::default()
            .with_username("alice")
            .with_password("secret")
            .with_channel_binding(ChannelBinding::Unsupported);
        let result = build_mechanism(SelectedMechanism::Plain, &creds);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name(), "PLAIN");
    }

    #[test]
    fn authentication_failed_is_non_retryable() {
        let error = ConnectionError::AuthenticationFailed("invalid credentials".to_string());
        assert!(!error.is_retryable());
    }

    #[test]
    fn selected_mechanism_display() {
        assert_eq!(SelectedMechanism::ScramSha256.to_string(), "SCRAM-SHA-256");
        assert_eq!(SelectedMechanism::ScramSha1.to_string(), "SCRAM-SHA-1");
        assert_eq!(SelectedMechanism::Plain.to_string(), "PLAIN");
    }
}

#[cfg(all(test, feature = "native"))]
mod native_tests {
    use tokio_xmpp::parsers::sasl::{DefinedCondition, Failure};

    use super::native::map_failure;
    use crate::error::ConnectionError;

    #[test]
    fn failure_maps_to_authentication_failed() {
        let failure = Failure {
            defined_condition: DefinedCondition::NotAuthorized,
            texts: Default::default(),
        };
        let error = map_failure(&failure);
        assert!(matches!(error, ConnectionError::AuthenticationFailed(_)));
        assert!(error.to_string().contains("NotAuthorized"));
    }

    #[test]
    fn failure_includes_text_when_present() {
        use std::collections::BTreeMap;

        let mut texts = BTreeMap::new();
        texts.insert("en".to_string(), "bad password".to_string());
        let failure = Failure {
            defined_condition: DefinedCondition::NotAuthorized,
            texts,
        };
        let error = map_failure(&failure);
        assert!(error.to_string().contains("bad password"));
    }

    #[test]
    fn temporary_auth_failure_maps_correctly() {
        let failure = Failure {
            defined_condition: DefinedCondition::TemporaryAuthFailure,
            texts: Default::default(),
        };
        let error = map_failure(&failure);
        assert!(matches!(error, ConnectionError::AuthenticationFailed(_)));
        assert!(error.to_string().contains("TemporaryAuthFailure"));
    }
}
