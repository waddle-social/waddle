//! The validated, immutable S2S configuration snapshot.
//!
//! A snapshot is built once, before any S2S socket exists, from the
//! operator's file and the certificate, key and CRL files it names. Every
//! check runs at load: a snapshot that loads is internally consistent, and
//! one that does not stops S2S from starting. Nothing re-reads the files
//! afterwards, so changing them changes nothing until the process restarts
//! with a new snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, CertificateRevocationListDer, PrivateKeyDer, UnixTime};
use rustls::sign::CertifiedKey;
use rustls::InconsistentKeys;
use uuid::Uuid;
use waddle_xmpp::identity::CanonicalDomain;

use super::admission::{is_test_domain, AdmissionMode, AdmissionPolicy};
use super::document::{
    AdmissionDocument, CredentialDocument, Document, EndpointDocument, LimitsDocument,
    ModeDocument, PrivateCaDocument,
};
use super::error::{ConfigError, ConfigSection};
use super::limits::{
    Deadlines, ResourceLimits, MAX_ENDPOINTS_PER_DOMAIN, MAX_ENDPOINT_DOMAINS, MAX_LIST_ENTRIES,
    MAX_LOCAL_DOMAINS, MAX_PRIVATE_CA_PROFILES, MAX_PROFILE_FILES,
};
use super::load::MaterialReader;
use crate::s2s::verify::{
    PrivateCaMaterial, PrivateCaProfile, PublicRoots, TlsConfigError, TlsSettings, TrustInventory,
    TrustProfileKind,
};

/// An endpoint configured for one remote domain, reached with STARTTLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfiguredEndpoint {
    pub address: SocketAddr,
}

/// The validated S2S configuration of one process.
pub struct S2sConfig {
    generation: Uuid,
    listen: Option<SocketAddr>,
    local_domains: Vec<CanonicalDomain>,
    credentials: BTreeMap<CanonicalDomain, Arc<CertifiedKey>>,
    admission: Arc<AdmissionPolicy>,
    trust: Arc<TrustInventory>,
    endpoints: BTreeMap<CanonicalDomain, Vec<ConfiguredEndpoint>>,
    limits: ResourceLimits,
    deadlines: Deadlines,
    settings: TlsSettings,
}

impl fmt::Debug for S2sConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Counts and the generation only: no key material, no domain.
        formatter
            .debug_struct("S2sConfig")
            .field("generation", &self.generation)
            .field("listen", &self.listen.is_some())
            .field("local_domains", &self.local_domains.len())
            .field("mode", &self.admission.mode())
            .field("endpoint_domains", &self.endpoints.len())
            .finish_non_exhaustive()
    }
}

impl S2sConfig {
    /// Load the snapshot named by `path`, verifying certificates and CRLs
    /// with `settings`' provider and clock.
    pub async fn load(path: &Path, settings: TlsSettings) -> Result<Self, ConfigError> {
        let mut reader = MaterialReader::default();
        let text = reader.read_config(path).await?;
        Self::from_toml(&text, &mut reader, settings).await
    }

    async fn from_toml(
        text: &str,
        reader: &mut MaterialReader,
        settings: TlsSettings,
    ) -> Result<Self, ConfigError> {
        let document: Document = toml::from_str(text)
            .map_err(|error| ConfigError::Syntax(error.message().to_owned()))?;
        Self::build(document, reader, settings).await
    }

    async fn build(
        document: Document,
        reader: &mut MaterialReader,
        settings: TlsSettings,
    ) -> Result<Self, ConfigError> {
        let generation =
            Uuid::parse_str(&document.generation).map_err(|_| ConfigError::InvalidGeneration)?;
        let now = settings.now().ok_or(ConfigError::ClockUnavailable)?;

        let local_domains = local_domains(&document.local_domains)?;
        let local: BTreeSet<CanonicalDomain> = local_domains.iter().cloned().collect();
        let admission = admission(&document.admission, &local)?;
        let listen = document
            .listen
            .as_deref()
            .map(|address| loopback_address(address, ConfigSection::Listen))
            .transpose()?;
        let credentials =
            credentials(&document.credentials, &local_domains, reader, &settings).await?;
        let trust = trust_inventory(&document.private_ca, &local, reader, &settings, now).await?;
        let admission = require_private_ca(admission, &document.admission, &trust)?;
        let endpoints = endpoints(&document.endpoints, &local)?;
        let (limits, deadlines) = limits(&document.limits)?;

        let expected: BTreeSet<&CanonicalDomain> = admission
            .allowlist()
            .iter()
            .chain(admission.trusted_homes())
            .chain(endpoints.keys())
            .collect();
        trust.summary(expected).log();

        Ok(Self {
            generation,
            listen,
            local_domains,
            credentials,
            admission: Arc::new(admission),
            trust: Arc::new(trust),
            endpoints,
            limits,
            deadlines,
            settings,
        })
    }

    /// The snapshot's identifier.
    pub fn generation(&self) -> Uuid {
        self.generation
    }

    /// The address of the inbound STARTTLS listener, when the process
    /// accepts S2S connections.
    pub fn listen(&self) -> Option<SocketAddr> {
        self.listen
    }

    /// The local domains, in configuration order.
    pub fn local_domains(&self) -> &[CanonicalDomain] {
        &self.local_domains
    }

    /// Whether `domain` is a local domain of this snapshot.
    pub fn is_local(&self, domain: &CanonicalDomain) -> bool {
        self.credentials.contains_key(domain)
    }

    /// The domain named in an error-carrying response header whose
    /// requested domain is not served: the first local domain.
    pub fn default_domain(&self) -> Option<&CanonicalDomain> {
        self.local_domains.first()
    }

    /// The credentials `domain` presents, when it is local.
    pub fn credentials(&self, domain: &CanonicalDomain) -> Option<Arc<CertifiedKey>> {
        self.credentials.get(domain).cloned()
    }

    /// Peer admission.
    pub fn admission(&self) -> &Arc<AdmissionPolicy> {
        &self.admission
    }

    /// The trust profiles of remote domains.
    pub fn trust(&self) -> &Arc<TrustInventory> {
        &self.trust
    }

    /// The configured endpoints of `domain`, in the order they are tried.
    pub fn endpoints(&self, domain: &CanonicalDomain) -> &[ConfiguredEndpoint] {
        self.endpoints.get(domain).map_or(&[], Vec::as_slice)
    }

    /// The caps and rates.
    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    /// The deadlines.
    pub fn deadlines(&self) -> &Deadlines {
        &self.deadlines
    }

    /// The TLS provider and clock.
    pub fn settings(&self) -> &TlsSettings {
        &self.settings
    }
}

/// Prepare every entry of a domain list as a federation peer domain.
fn peer_domains(
    section: ConfigSection,
    entries: &[String],
) -> Result<Vec<CanonicalDomain>, ConfigError> {
    if entries.len() > MAX_LIST_ENTRIES {
        return Err(ConfigError::TooManyEntries(section));
    }
    let mut seen = BTreeSet::new();
    let mut domains = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let domain = peer_domain(section, index, entry)?;
        if !seen.insert(domain.clone()) {
            return Err(ConfigError::Duplicate { section, index });
        }
        domains.push(domain);
    }
    Ok(domains)
}

fn peer_domain(
    section: ConfigSection,
    index: usize,
    entry: &str,
) -> Result<CanonicalDomain, ConfigError> {
    let domain = CanonicalDomain::prepare(entry).map_err(|error| ConfigError::InvalidDomain {
        section,
        index,
        error,
    })?;
    if !domain.kind().names_federation_peer() {
        return Err(ConfigError::NotAPeerDomain { section, index });
    }
    // The discovery slice lifts this restriction through its own reviewed
    // change (ADR-0020 gate G6).
    if !is_test_domain(&domain) {
        return Err(ConfigError::OutsideTestDomain { section, index });
    }
    Ok(domain)
}

fn local_domains(entries: &[String]) -> Result<Vec<CanonicalDomain>, ConfigError> {
    if entries.is_empty() {
        return Err(ConfigError::NoLocalDomains);
    }
    if entries.len() > MAX_LOCAL_DOMAINS {
        return Err(ConfigError::TooManyEntries(ConfigSection::LocalDomains));
    }
    peer_domains(ConfigSection::LocalDomains, entries)
}

/// A remote-domain list whose entries must not be local.
fn remote_domains(
    section: ConfigSection,
    entries: &[String],
    local: &BTreeSet<CanonicalDomain>,
) -> Result<BTreeSet<CanonicalDomain>, ConfigError> {
    let domains = peer_domains(section, entries)?;
    if let Some(index) = domains.iter().position(|domain| local.contains(domain)) {
        return Err(ConfigError::LocalDomain { section, index });
    }
    Ok(domains.into_iter().collect())
}

fn admission(
    document: &AdmissionDocument,
    local: &BTreeSet<CanonicalDomain>,
) -> Result<AdmissionPolicy, ConfigError> {
    let mode = match document.mode {
        None | Some(ModeDocument::Allowlist) => AdmissionMode::Allowlist,
        Some(ModeDocument::Closed) => AdmissionMode::Closed,
        Some(ModeDocument::Open) => AdmissionMode::Open,
    };
    let deny_entries = peer_domains(ConfigSection::Denylist, &document.deny)?;
    // A denylist entry covering a local domain would refuse this
    // deployment's own name; it is a contradiction, not a policy.
    if let Some(index) = deny_entries
        .iter()
        .position(|entry| local.iter().any(|domain| domain.is_within(entry)))
    {
        return Err(ConfigError::LocalDomain {
            section: ConfigSection::Denylist,
            index,
        });
    }
    let deny: BTreeSet<CanonicalDomain> = deny_entries.into_iter().collect();
    let allow = exact_list(ConfigSection::Allowlist, &document.allow, local, &deny)?;
    let dialback = exact_list(
        ConfigSection::DialbackList,
        &document.dialback,
        local,
        &deny,
    )?;
    let trusted_homes = exact_list(
        ConfigSection::TrustedHomes,
        &document.trusted_homes,
        local,
        &deny,
    )?;
    Ok(AdmissionPolicy::new(
        mode,
        local.clone(),
        allow,
        deny,
        dialback,
        trusted_homes,
    ))
}

/// An allowlist, dialback or trusted-home list: exact entries that name no
/// local domain and that no denylist entry covers.
fn exact_list(
    section: ConfigSection,
    entries: &[String],
    local: &BTreeSet<CanonicalDomain>,
    deny: &BTreeSet<CanonicalDomain>,
) -> Result<BTreeSet<CanonicalDomain>, ConfigError> {
    let domains = peer_domains(section, entries)?;
    for (index, domain) in domains.iter().enumerate() {
        if local.contains(domain) {
            return Err(ConfigError::LocalDomain { section, index });
        }
        // The denylist overrides every other list, and a configuration
        // that lists a domain it covers is contradictory (ADR-0020 D1).
        if deny.iter().any(|entry| domain.is_within(entry)) {
            return Err(ConfigError::Denied { section, index });
        }
    }
    Ok(domains.into_iter().collect())
}

/// Mark the trusted homes that must authenticate under a private-CA
/// profile. Each must be a trusted home and have such a profile.
fn require_private_ca(
    admission: AdmissionPolicy,
    document: &AdmissionDocument,
    trust: &TrustInventory,
) -> Result<AdmissionPolicy, ConfigError> {
    let section = ConfigSection::RequirePrivateCa;
    let domains = peer_domains(section, &document.require_private_ca)?;
    for (index, domain) in domains.iter().enumerate() {
        if !admission.trusted_homes().contains(domain) {
            return Err(ConfigError::NotTrustedHome { index });
        }
        if trust.profile_for(domain).kind() != TrustProfileKind::PrivateCa {
            return Err(ConfigError::NoPrivateCaProfile { index });
        }
    }
    Ok(admission.with_private_ca_required(domains.into_iter().collect()))
}

fn loopback_address(text: &str, section: ConfigSection) -> Result<SocketAddr, ConfigError> {
    let address: SocketAddr = text
        .parse()
        .map_err(|_| ConfigError::InvalidAddress(section))?;
    // The discovery slice lifts this restriction through its own reviewed
    // change (ADR-0020 gate G6).
    if !address.ip().is_loopback() {
        return Err(ConfigError::NotLoopback(section));
    }
    Ok(address)
}

async fn credentials(
    documents: &[CredentialDocument],
    local_domains: &[CanonicalDomain],
    reader: &mut MaterialReader,
    settings: &TlsSettings,
) -> Result<BTreeMap<CanonicalDomain, Arc<CertifiedKey>>, ConfigError> {
    let section = ConfigSection::Credentials;
    if documents.len() > MAX_LOCAL_DOMAINS {
        return Err(ConfigError::TooManyEntries(section));
    }
    let mut credentials = BTreeMap::new();
    for (index, document) in documents.iter().enumerate() {
        let domain = CanonicalDomain::prepare(&document.domain).map_err(|error| {
            ConfigError::InvalidDomain {
                section,
                index,
                error,
            }
        })?;
        if !local_domains.contains(&domain) {
            return Err(ConfigError::UnservedCredentials { index });
        }
        if credentials.contains_key(&domain) {
            return Err(ConfigError::Duplicate { section, index });
        }
        let chain_pem = reader.read(&document.certificate_chain, section).await?;
        let chain = CertificateDer::pem_slice_iter(&chain_pem[..])
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ConfigError::NoPem {
                section,
                expected: "certificate",
            })?;
        if chain.is_empty() {
            return Err(ConfigError::EmptyCertificateChain { index });
        }
        let key_pem = reader.read(&document.private_key, section).await?;
        let key = PrivateKeyDer::from_pem_slice(&key_pem[..])
            .map_err(|_| ConfigError::InvalidPrivateKey { index })?;
        let signing_key = settings
            .provider()
            .key_provider
            .load_private_key(key)
            .map_err(|_| ConfigError::InvalidPrivateKey { index })?;
        let certified = CertifiedKey::new(chain, signing_key);
        match certified.keys_match() {
            Ok(()) | Err(rustls::Error::InconsistentKeys(InconsistentKeys::Unknown)) => {}
            Err(rustls::Error::InconsistentKeys(InconsistentKeys::KeyMismatch)) => {
                return Err(ConfigError::Credentials {
                    index,
                    error: TlsConfigError::KeyMismatch,
                })
            }
            Err(error) => {
                return Err(ConfigError::Credentials {
                    index,
                    error: TlsConfigError::Credentials(error),
                })
            }
        }
        credentials.insert(domain, Arc::new(certified));
    }
    if let Some(index) = local_domains
        .iter()
        .position(|domain| !credentials.contains_key(domain))
    {
        return Err(ConfigError::MissingCredentials { index });
    }
    Ok(credentials)
}

async fn trust_inventory(
    documents: &[PrivateCaDocument],
    local: &BTreeSet<CanonicalDomain>,
    reader: &mut MaterialReader,
    settings: &TlsSettings,
    now: UnixTime,
) -> Result<TrustInventory, ConfigError> {
    let section = ConfigSection::PrivateCa;
    if documents.len() > MAX_PRIVATE_CA_PROFILES {
        return Err(ConfigError::TooManyEntries(section));
    }
    // Operator configuration cannot substitute the public roots: a domain
    // that must not trust them gets a private-CA profile instead.
    let mut inventory = TrustInventory::new(PublicRoots::mozilla());
    let mut configured = BTreeSet::new();
    for (index, document) in documents.iter().enumerate() {
        let domains = remote_domains(section, &document.domains, local)?;
        if domains.is_empty() {
            return Err(ConfigError::EmptyProfile { index });
        }
        let files = document.anchors.len() + document.intermediates.len() + document.crls.len();
        if files > MAX_PROFILE_FILES {
            return Err(ConfigError::TooManyEntries(section));
        }
        let mut material = PrivateCaMaterial::default();
        for path in &document.anchors {
            material
                .anchors
                .extend(read_certificates(reader, path, section).await?);
        }
        for path in &document.intermediates {
            material
                .intermediates
                .extend(read_certificates(reader, path, section).await?);
        }
        for path in &document.crls {
            let pem = reader.read(path, section).await?;
            let crls = CertificateRevocationListDer::pem_slice_iter(&pem[..])
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| ConfigError::NoPem {
                    section,
                    expected: "CRL",
                })?;
            if crls.is_empty() {
                return Err(ConfigError::NoPem {
                    section,
                    expected: "CRL",
                });
            }
            material.crls.extend(crls);
        }
        let profile = PrivateCaProfile::load(&material, &settings.algorithms(), now)
            .map_err(|error| ConfigError::Trust { index, error })?;
        let profile = Arc::new(profile);
        for domain in domains {
            if !configured.insert(domain.clone()) {
                return Err(ConfigError::Duplicate { section, index });
            }
            inventory
                .configure_private_ca(domain, Arc::clone(&profile))
                .map_err(|error| ConfigError::Trust { index, error })?;
        }
    }
    Ok(inventory)
}

async fn read_certificates(
    reader: &mut MaterialReader,
    path: &Path,
    section: ConfigSection,
) -> Result<Vec<CertificateDer<'static>>, ConfigError> {
    let pem = reader.read(path, section).await?;
    let certificates = CertificateDer::pem_slice_iter(&pem[..])
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ConfigError::NoPem {
            section,
            expected: "certificate",
        })?;
    if certificates.is_empty() {
        return Err(ConfigError::NoPem {
            section,
            expected: "certificate",
        });
    }
    Ok(certificates)
}

fn endpoints(
    documents: &[EndpointDocument],
    local: &BTreeSet<CanonicalDomain>,
) -> Result<BTreeMap<CanonicalDomain, Vec<ConfiguredEndpoint>>, ConfigError> {
    let section = ConfigSection::Endpoints;
    let mut endpoints: BTreeMap<CanonicalDomain, Vec<ConfiguredEndpoint>> = BTreeMap::new();
    for (index, document) in documents.iter().enumerate() {
        let domain = peer_domain(section, index, &document.domain)?;
        if local.contains(&domain) {
            return Err(ConfigError::LocalDomain { section, index });
        }
        let address = loopback_address(&document.address, section)?;
        if !endpoints.contains_key(&domain) && endpoints.len() >= MAX_ENDPOINT_DOMAINS {
            return Err(ConfigError::TooManyEntries(section));
        }
        let list = endpoints.entry(domain).or_default();
        if list.len() >= MAX_ENDPOINTS_PER_DOMAIN {
            return Err(ConfigError::TooManyEntries(section));
        }
        let endpoint = ConfiguredEndpoint { address };
        if list.contains(&endpoint) {
            return Err(ConfigError::Duplicate { section, index });
        }
        list.push(endpoint);
    }
    Ok(endpoints)
}

/// A lowered count: present values must be within `1..=ceiling`.
fn count(name: &'static str, value: Option<usize>, ceiling: usize) -> Result<usize, ConfigError> {
    match value {
        None => Ok(ceiling),
        Some(value) if value > 0 && value <= ceiling => Ok(value),
        Some(_) => Err(ConfigError::LimitOutOfRange(name)),
    }
}

/// A lowered rate: present values must be within `1..=ceiling`.
fn rate(name: &'static str, value: Option<u32>, ceiling: u32) -> Result<u32, ConfigError> {
    match value {
        None => Ok(ceiling),
        Some(value) if value > 0 && value <= ceiling => Ok(value),
        Some(_) => Err(ConfigError::LimitOutOfRange(name)),
    }
}

/// A duration in milliseconds within `1..=ceiling`, or the default.
fn duration(
    name: &'static str,
    value: Option<u64>,
    default: Duration,
    ceiling: Duration,
) -> Result<Duration, ConfigError> {
    match value {
        None => Ok(default),
        Some(millis) => {
            let value = Duration::from_millis(millis);
            if millis > 0 && value <= ceiling {
                Ok(value)
            } else {
                Err(ConfigError::LimitOutOfRange(name))
            }
        }
    }
}

fn limits(document: &LimitsDocument) -> Result<(ResourceLimits, Deadlines), ConfigError> {
    let ceiling = ResourceLimits::CEILING;
    let mut limits = ResourceLimits {
        max_sockets: count("max_sockets", document.max_sockets, ceiling.max_sockets)?,
        max_inbound_sockets: count(
            "max_inbound_sockets",
            document.max_inbound_sockets,
            ceiling.max_inbound_sockets,
        )?,
        max_outbound_sockets: count(
            "max_outbound_sockets",
            document.max_outbound_sockets,
            ceiling.max_outbound_sockets,
        )?,
        max_unauthenticated: count(
            "max_unauthenticated",
            document.max_unauthenticated,
            ceiling.max_unauthenticated,
        )?,
        max_unauthenticated_per_address: count(
            "max_unauthenticated_per_address",
            document.max_unauthenticated_per_address,
            ceiling.max_unauthenticated_per_address,
        )?,
        max_inbound_handshakes: count(
            "max_inbound_handshakes",
            document.max_inbound_handshakes,
            ceiling.max_inbound_handshakes,
        )?,
        max_outbound_handshakes: count(
            "max_outbound_handshakes",
            document.max_outbound_handshakes,
            ceiling.max_outbound_handshakes,
        )?,
        max_dialing: count("max_dialing", document.max_dialing, ceiling.max_dialing)?,
        max_backoff_destinations: count(
            "max_backoff_destinations",
            document.max_backoff_destinations,
            ceiling.max_backoff_destinations,
        )?,
        max_streams: count("max_streams", document.max_streams, ceiling.max_streams)?,
        max_peers: count("max_peers", document.max_peers, ceiling.max_peers)?,
        max_negotiating_per_peer: count(
            "max_negotiating_per_peer",
            document.max_negotiating_per_peer,
            ceiling.max_negotiating_per_peer,
        )?,
        ..ceiling
    };
    limits.accept_rate.per_second = rate(
        "accept_per_second",
        document.accept_per_second,
        ceiling.accept_rate.per_second,
    )?;
    limits.accept_rate.burst = rate(
        "accept_burst",
        document.accept_burst,
        ceiling.accept_rate.burst,
    )?;
    limits.accept_rate_per_address.per_second = rate(
        "accept_per_address_per_second",
        document.accept_per_address_per_second,
        ceiling.accept_rate_per_address.per_second,
    )?;
    limits.accept_rate_per_address.burst = rate(
        "accept_per_address_burst",
        document.accept_per_address_burst,
        ceiling.accept_rate_per_address.burst,
    )?;

    let defaults = Deadlines::DEFAULT;
    let ceilings = Deadlines::CEILING;
    let deadlines = Deadlines {
        connect: duration(
            "connect_timeout_ms",
            document.connect_timeout_ms,
            defaults.connect,
            ceilings.connect,
        )?,
        stage: duration(
            "stage_timeout_ms",
            document.stage_timeout_ms,
            defaults.stage,
            ceilings.stage,
        )?,
        sasl: duration(
            "sasl_timeout_ms",
            document.sasl_timeout_ms,
            defaults.sasl,
            ceilings.sasl,
        )?,
        negotiation: duration(
            "negotiation_timeout_ms",
            document.negotiation_timeout_ms,
            defaults.negotiation,
            ceilings.negotiation,
        )?,
        incomplete_element: duration(
            "incomplete_element_timeout_ms",
            document.incomplete_element_timeout_ms,
            defaults.incomplete_element,
            ceilings.incomplete_element,
        )?,
        idle: duration(
            "idle_timeout_ms",
            document.idle_timeout_ms,
            defaults.idle,
            ceilings.idle,
        )?,
        max_age: duration(
            "max_age_ms",
            document.max_age_ms,
            defaults.max_age,
            ceilings.max_age,
        )?,
        write: duration(
            "write_timeout_ms",
            document.write_timeout_ms,
            defaults.write,
            ceilings.write,
        )?,
        drain: duration(
            "drain_timeout_ms",
            document.drain_timeout_ms,
            defaults.drain,
            ceilings.drain,
        )?,
        eviction_idle: duration(
            "eviction_idle_ms",
            document.eviction_idle_ms,
            defaults.eviction_idle,
            ceilings.eviction_idle,
        )?,
        replacement_interval: duration(
            "replacement_interval_ms",
            document.replacement_interval_ms,
            defaults.replacement_interval,
            ceilings.replacement_interval,
        )?,
        backoff_initial: duration(
            "backoff_initial_ms",
            document.backoff_initial_ms,
            defaults.backoff_initial,
            ceilings.backoff_initial,
        )?,
        backoff_max: duration(
            "backoff_max_ms",
            document.backoff_max_ms,
            defaults.backoff_max,
            ceilings.backoff_max,
        )?,
    };
    if deadlines.backoff_initial > deadlines.backoff_max {
        return Err(ConfigError::LimitOutOfRange("backoff_initial_ms"));
    }
    Ok((limits, deadlines))
}
