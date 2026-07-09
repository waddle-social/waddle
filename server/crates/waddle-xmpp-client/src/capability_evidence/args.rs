use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::Parser;
use jid::BareJid;
use url::Url;

use crate::{
    AccessToken, ClientConfig, ClientResource, DiscoveryExt, OAuthBearerConfig, WebSocketConfig,
    XmppClient,
};

use super::collect::{
    collect_live_disco_with, CollectionProvenance, CollectionWindow, QueryFailure, TargetInputs,
};
use super::contract::LoadedTargetContract;
use super::model::*;
use super::output::write_artifact;

#[derive(Debug, Parser)]
#[command(name = "waddle-capability-collector")]
#[command(about = "Collect privacy-minimized live XMPP capability evidence")]
pub struct CapabilityCollectorArgs {
    #[arg(long)]
    endpoint: String,
    #[arg(long)]
    origin: Option<String>,
    #[arg(long)]
    xmpp_domain: String,
    #[arg(long, default_value = "WADDLE_CAPABILITY_ACCOUNT_JID")]
    account_env: String,
    #[arg(long)]
    muc_domain: String,
    #[arg(long)]
    spaces_domain: String,
    #[arg(long)]
    representative_muc_room_env: Option<String>,
    #[arg(long)]
    calls_configured: bool,
    #[arg(long, default_value = "waddle-capability-collector")]
    resource: String,
    #[arg(long, default_value = "WADDLE_CAPABILITY_ACCESS_TOKEN")]
    access_token_env: String,
    #[arg(long)]
    server_commit: String,
    #[arg(long)]
    window_start: String,
    #[arg(long)]
    window_end: String,
    #[arg(long)]
    job: String,
    #[arg(long)]
    environment: String,
    #[arg(long)]
    cluster: String,
    #[arg(long)]
    namespace: String,
    #[arg(long)]
    expected_replicas: u32,
    #[arg(long, default_value = DEFAULT_IDENTITY_METRIC)]
    identity_metric: String,
    #[arg(long, default_value = DEFAULT_TARGET_SIGNAL_ID)]
    target_signal_id: String,
    #[arg(long, default_value_t = DEFAULT_IDENTITY_LOOKBACK_SECONDS)]
    identity_lookback_seconds: u32,
    #[arg(long, default_value_t = 15)]
    request_timeout_seconds: u64,
    #[arg(long)]
    target_contract: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

struct ValidatedCollectorArgs {
    endpoint: Url,
    origin: Option<Url>,
    resource: ClientResource,
    access_token_env: String,
    target_inputs: TargetInputs,
    provenance: CollectionProvenance,
    request_timeout: Duration,
    target_contract: PathBuf,
    output: PathBuf,
}

impl CapabilityCollectorArgs {
    fn validate(self) -> Result<ValidatedCollectorArgs, CapabilityEvidenceError> {
        let server_commit = GitCommit::parse(self.server_commit)?;
        let xmpp_domain = parse_bare_jid(&self.xmpp_domain)?;
        let muc_domain = parse_bare_jid(&self.muc_domain)?;
        let spaces_domain = parse_bare_jid(&self.spaces_domain)?;
        if xmpp_domain.node().is_some()
            || muc_domain.node().is_some()
            || spaces_domain.node().is_some()
        {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "XMPP service domains must not contain a localpart",
            ));
        }
        let endpoint = Url::parse(&self.endpoint)
            .map_err(|_| CapabilityEvidenceError::InvalidArgument("invalid WebSocket URL"))?;
        if endpoint.scheme() != "wss"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "live capability collection requires a credential-free wss endpoint",
            ));
        }
        let endpoint_host = endpoint
            .host_str()
            .ok_or(CapabilityEvidenceError::InvalidArgument(
                "WebSocket endpoint must have a host",
            ))?;
        let xmpp_domain_name = xmpp_domain.domain().as_str();
        if endpoint_host != xmpp_domain_name
            && !endpoint_host
                .strip_suffix(xmpp_domain_name)
                .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.'))
        {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "WebSocket endpoint host must be the XMPP domain or one of its subdomains",
            ));
        }
        let origin = self
            .origin
            .map(|origin| {
                Url::parse(&origin)
                    .map_err(|_| CapabilityEvidenceError::InvalidArgument("invalid origin URL"))
            })
            .transpose()?;
        if origin.as_ref().is_some_and(|origin| {
            origin.scheme() != "https"
                || !origin.username().is_empty()
                || origin.password().is_some()
                || origin.query().is_some()
                || origin.fragment().is_some()
                || origin.path() != "/"
                || origin.host_str() != endpoint.host_str()
                || origin.port_or_known_default() != endpoint.port_or_known_default()
        }) {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "origin must be credential-free HTTPS on the WebSocket endpoint host and port",
            ));
        }
        validate_environment_name(&self.account_env)?;
        if let Some(name) = &self.representative_muc_room_env {
            validate_environment_name(name)?;
        }
        validate_environment_name(&self.access_token_env)?;
        let account = read_jid_environment(&self.account_env, true)?.ok_or(
            CapabilityEvidenceError::InvalidArgument("account environment value is required"),
        )?;
        let representative_muc_room = self
            .representative_muc_room_env
            .as_deref()
            .map(|name| read_jid_environment(name, false))
            .transpose()?
            .flatten();
        let target_inputs = TargetInputs {
            xmpp_domain,
            muc_domain,
            spaces_domain,
            account,
            representative_muc_room,
            calls_configured: self.calls_configured,
        };
        target_inputs.validate()?;
        let window = CollectionWindow {
            start: parse_utc(&self.window_start)?,
            end: parse_utc(&self.window_end)?,
        };
        if window.end <= window.start {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "window end must be later than its start",
            ));
        }
        if window.end.signed_duration_since(window.start) < chrono::Duration::hours(1) {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "capability evidence window must be at least 60 minutes",
            ));
        }
        if self.expected_replicas == 0
            || self.expected_replicas > 10_000
            || self.identity_lookback_seconds == 0
            || !self.identity_lookback_seconds.is_multiple_of(60)
            || !(1..=60).contains(&self.request_timeout_seconds)
        {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "invalid replica, lookback, or timeout value",
            ));
        }
        Ok(ValidatedCollectorArgs {
            endpoint,
            origin,
            resource: ClientResource::new(self.resource)
                .map_err(|_| CapabilityEvidenceError::InvalidArgument("invalid resource"))?,
            access_token_env: self.access_token_env,
            target_inputs,
            provenance: CollectionProvenance {
                server_commit,
                deployment_scope: DeploymentScope {
                    job: ScopeLabel::parse(self.job)?,
                    environment: ScopeLabel::parse(self.environment)?,
                    cluster: ScopeLabel::parse(self.cluster)?,
                    namespace: ScopeLabel::parse(self.namespace)?,
                    expected_replicas: self.expected_replicas,
                    identity_metric: MetricName::parse(self.identity_metric)?,
                    target_signal_id: SignalId::parse(self.target_signal_id)?,
                    identity_lookback_seconds: self.identity_lookback_seconds,
                },
                window,
            },
            request_timeout: Duration::from_secs(self.request_timeout_seconds),
            target_contract: self.target_contract,
            output: self.output,
        })
    }
}

fn parse_bare_jid(value: &str) -> Result<BareJid, CapabilityEvidenceError> {
    BareJid::from_str(value)
        .map_err(|_| CapabilityEvidenceError::InvalidArgument("invalid bare JID"))
}

fn validate_environment_name(value: &str) -> Result<(), CapabilityEvidenceError> {
    (!value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'))
    .then_some(())
    .ok_or(CapabilityEvidenceError::InvalidArgument(
        "collector environment names must be uppercase",
    ))
}

fn read_jid_environment(
    name: &str,
    required: bool,
) -> Result<Option<BareJid>, CapabilityEvidenceError> {
    match std::env::var_os(name) {
        Some(value) => value
            .into_string()
            .ok()
            .and_then(|value| BareJid::from_str(&value).ok())
            .map(Some)
            .ok_or(CapabilityEvidenceError::InvalidArgument(
                "JID environment value is invalid",
            )),
        None if required => Err(CapabilityEvidenceError::InvalidArgument(
            "required JID environment value is missing",
        )),
        None => Ok(None),
    }
}

pub(super) fn parse_utc(value: &str) -> Result<DateTime<Utc>, CapabilityEvidenceError> {
    if !value.ends_with('Z') {
        return Err(CapabilityEvidenceError::InvalidArgument(
            "timestamps must be RFC 3339 UTC instants",
        ));
    }
    DateTime::parse_from_rfc3339(value)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| {
            CapabilityEvidenceError::InvalidArgument("timestamps must be RFC 3339 UTC instants")
        })
}

pub async fn run_capability_collector(
    args: CapabilityCollectorArgs,
) -> Result<PathBuf, CapabilityEvidenceError> {
    let args = args.validate()?;
    let contract_bytes = std::fs::read(&args.target_contract)?;
    let contract = LoadedTargetContract::from_bytes(&contract_bytes)?;
    let token = std::env::var_os(&args.access_token_env)
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .ok_or(CapabilityEvidenceError::MissingAccessToken)?;

    let mut transport = WebSocketConfig::new(args.endpoint)
        .map_err(|_| CapabilityEvidenceError::InvalidArgument("invalid WebSocket config"))?;
    transport.origin = args.origin;
    let client_config = ClientConfig::new(
        crate::ConnectionConfig::new(args.target_inputs.xmpp_domain.clone()),
        transport,
        OAuthBearerConfig::new(
            args.target_inputs.account.clone(),
            args.resource,
            AccessToken::new(token),
        )
        .map_err(|_| CapabilityEvidenceError::InvalidArgument("invalid authentication config"))?,
    )
    .map_err(|_| CapabilityEvidenceError::InvalidArgument("invalid client config"))?;
    let handle = XmppClient::new(client_config)
        .and_then(|client| client.driver())
        .map_err(|_| CapabilityEvidenceError::ConnectionFailed)?
        .connect()
        .await
        .map_err(|_| CapabilityEvidenceError::ConnectionFailed)?;
    let request_timeout = args.request_timeout;
    let artifact = collect_live_disco_with(
        &contract,
        &args.target_inputs,
        &args.provenance,
        |_, jid| {
            let handle = handle.clone();
            async move {
                match tokio::time::timeout(request_timeout, handle.discover_info_for(&jid)).await {
                    Ok(Ok(result)) => Ok(result),
                    Ok(Err(_)) => Err(QueryFailure::Failed),
                    Err(_) => Err(QueryFailure::TimedOut),
                }
            }
        },
        Utc::now,
    )
    .await;
    let _ = handle.disconnect().await;
    let artifact = artifact?;
    write_artifact(&args.output, &artifact.to_pretty_json()?)?;
    Ok(args.output)
}
