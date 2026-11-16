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

use super::admission::{is_test_domain, AdmissionMode, AdmissionPolicy, PeerScope};
use super::document::{
    AdmissionDocument, CredentialDocument, DialbackDocument, DiscoveryDocument, Document,
    EndpointDocument, EndpointTransportDocument, LimitsDocument, ModeDocument, PeerScopeDocument,
    PrivateCaDocument,
};
use super::error::{ConfigError, ConfigSection};
use super::limits::{
    Deadlines, ResourceLimits, MAX_BLOCKED_PREFIXES, MAX_ENDPOINTS_PER_DOMAIN,
    MAX_ENDPOINT_DOMAINS, MAX_LIST_ENTRIES, MAX_LOCAL_DOMAINS, MAX_PRIVATE_CA_PROFILES,
    MAX_PROFILE_FILES, MIN_INPUT_BYTES_PER_SECOND,
};
use super::load::MaterialReader;
use crate::s2s::dialback::DialbackSecret;
use crate::s2s::discovery::address::{AddressBlock, BlockedAddresses};
use crate::s2s::verify::{
    OutboundTransport, PrivateCaMaterial, PrivateCaProfile, PublicRoots, TlsConfigError,
    TlsSettings, TrustInventory, TrustProfileKind,
};

/// The shortest dialback secret accepted, in bytes.
pub const MIN_DIALBACK_SECRET_BYTES: usize = 32;

/// An endpoint configured for one remote domain (RFC 6120 §3.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfiguredEndpoint {
    pub address: SocketAddr,
    /// STARTTLS, or XEP-0368 direct TLS.
    pub transport: OutboundTransport,
}

/// Which Server Dialback roles run (ADR-0020 D2). Every role is off by
/// default.
#[derive(Clone, Default)]
pub struct DialbackSettings {
    /// The Initiating and Authoritative Server roles, with the deployment's
    /// secret: present exactly when `dialback.outbound` is on.
    secret: Option<Arc<DialbackSecret>>,
    /// The Receiving Server role for domains on the dialback list that are
    /// admitted.
    inbound: bool,
}

impl DialbackSettings {
    /// The secret, when this deployment authenticates itself by dialback
    /// and answers verification requests.
    pub fn secret(&self) -> Option<&Arc<DialbackSecret>> {
        self.secret.as_ref()
    }

    /// Whether the Initiating and Authoritative Server roles run.
    pub fn outbound(&self) -> bool {
        self.secret.is_some()
    }

    /// Whether the Receiving Server role runs.
    pub fn inbound(&self) -> bool {
        self.inbound
    }
}

impl fmt::Debug for DialbackSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DialbackSettings")
            .field("outbound", &self.outbound())
            .field("inbound", &self.inbound)
            .finish()
    }
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
    discovery: bool,
    blocked: Arc<BlockedAddresses>,
    dialback: DialbackSettings,
    grants_require_pkix: bool,
    federate_new_rooms: bool,
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
            .field("scope", &self.admission.scope())
            .field("endpoint_domains", &self.endpoints.len())
            .field("discovery", &self.discovery)
            .field("blocked_prefixes", &self.blocked.extra().len())
            .field("dialback", &self.dialback)
            .field("grants_require_pkix", &self.grants_require_pkix)
            .field("federate_new_rooms", &self.federate_new_rooms)
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

        let scope = match document.peer_scope {
            None | Some(PeerScopeDocument::Test) => PeerScope::Test,
            Some(PeerScopeDocument::Public) => PeerScope::Public,
        };
        let local_domains = local_domains(&document.local_domains, scope)?;
        let local: BTreeSet<CanonicalDomain> = local_domains.iter().cloned().collect();
        let admission = admission(&document.admission, &local, scope)?;
        let listen = document
            .listen
            .as_deref()
            .map(|address| socket_address(address, ConfigSection::Listen, scope))
            .transpose()?;
        let credentials =
            credentials(&document.credentials, &local_domains, reader, &settings).await?;
        let trust =
            trust_inventory(&document.private_ca, &local, reader, &settings, now, scope).await?;
        let admission = require_private_ca(admission, &document.admission, &trust, scope)?;
        let endpoints = endpoints(&document.endpoints, &local, scope)?;
        let (limits, deadlines) = limits(&document.limits)?;
        let blocked = discovery(&document.discovery, scope)?;
        let dialback = dialback(&document.dialback, &admission, reader).await?;

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
            discovery: document.discovery.enabled,
            blocked: Arc::new(blocked),
            dialback,
            grants_require_pkix: document.host_policy.require_pkix,
            federate_new_rooms: document.host_policy.federate_new_rooms,
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

    /// Whether remote domains without configured endpoints are discovered
    /// through DNS (ADR-0020 D3).
    pub fn discovery_enabled(&self) -> bool {
        self.discovery
    }

    /// The address classes refused for DNS-derived addresses: the built-in
    /// classes and the operator's `discovery.blocked_prefixes`.
    pub fn blocked_addresses(&self) -> &Arc<BlockedAddresses> {
        &self.blocked
    }

    /// The Server Dialback roles.
    pub fn dialback(&self) -> &DialbackSettings {
        &self.dialback
    }

    /// Whether an inbound stream from `remote` may authenticate by
    /// dialback: the Receiving Server role runs, and `remote` is on the
    /// dialback list and admitted (ADR-0020 D2).
    pub fn permits_inbound_dialback(&self, remote: &CanonicalDomain) -> bool {
        self.dialback.inbound && self.admission.permits_dialback(remote)
    }

    /// Whether host policy honours a remote principal's grants only on a
    /// stream with `pkix` strength, member-level grants included
    /// (`host_policy.require_pkix`). Privileged grants and room moderation
    /// need `pkix` whatever this says (ADR-0020 D8).
    pub fn grants_require_pkix(&self) -> bool {
        self.grants_require_pkix
    }

    /// Whether a room created while this snapshot runs starts with its
    /// `federated` setting on (`host_policy.federate_new_rooms`). Off by
    /// default; it never changes a room that exists.
    pub fn federates_new_rooms(&self) -> bool {
        self.federate_new_rooms
    }
}

/// Prepare every entry of a domain list as a federation peer domain, within
/// `.test` when `scope` is [`PeerScope::Test`].
fn peer_domains(
    section: ConfigSection,
    entries: &[String],
    scope: PeerScope,
) -> Result<Vec<CanonicalDomain>, ConfigError> {
    if entries.len() > MAX_LIST_ENTRIES {
        return Err(ConfigError::TooManyEntries(section));
    }
    let mut seen = BTreeSet::new();
    let mut domains = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let domain = peer_domain(section, index, entry, scope)?;
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
    scope: PeerScope,
) -> Result<CanonicalDomain, ConfigError> {
    let domain = CanonicalDomain::prepare(entry).map_err(|error| ConfigError::InvalidDomain {
        section,
        index,
        error,
    })?;
    if !domain.kind().names_federation_peer() {
        return Err(ConfigError::NotAPeerDomain { section, index });
    }
    // Lifted only by `peer_scope = "public"` (ADR-0020 gate G6).
    if scope == PeerScope::Test && !is_test_domain(&domain) {
        return Err(ConfigError::OutsideTestDomain { section, index });
    }
    Ok(domain)
}

fn local_domains(
    entries: &[String],
    scope: PeerScope,
) -> Result<Vec<CanonicalDomain>, ConfigError> {
    if entries.is_empty() {
        return Err(ConfigError::NoLocalDomains);
    }
    if entries.len() > MAX_LOCAL_DOMAINS {
        return Err(ConfigError::TooManyEntries(ConfigSection::LocalDomains));
    }
    peer_domains(ConfigSection::LocalDomains, entries, scope)
}

/// A remote-domain list whose entries must not be local.
fn remote_domains(
    section: ConfigSection,
    entries: &[String],
    local: &BTreeSet<CanonicalDomain>,
    scope: PeerScope,
) -> Result<BTreeSet<CanonicalDomain>, ConfigError> {
    let domains = peer_domains(section, entries, scope)?;
    if let Some(index) = domains.iter().position(|domain| local.contains(domain)) {
        return Err(ConfigError::LocalDomain { section, index });
    }
    Ok(domains.into_iter().collect())
}

fn admission(
    document: &AdmissionDocument,
    local: &BTreeSet<CanonicalDomain>,
    scope: PeerScope,
) -> Result<AdmissionPolicy, ConfigError> {
    let mode = match document.mode {
        None | Some(ModeDocument::Allowlist) => AdmissionMode::Allowlist,
        Some(ModeDocument::Closed) => AdmissionMode::Closed,
        Some(ModeDocument::Open) => AdmissionMode::Open,
    };
    // Denylist entries only refuse, so they may name any DNS domain in
    // every scope and an operator can pre-provision real domains.
    let deny_entries = peer_domains(ConfigSection::Denylist, &document.deny, PeerScope::Public)?;
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
    let allow = exact_list(
        ConfigSection::Allowlist,
        &document.allow,
        local,
        &deny,
        scope,
    )?;
    let dialback = exact_list(
        ConfigSection::DialbackList,
        &document.dialback,
        local,
        &deny,
        scope,
    )?;
    let trusted_homes = exact_list(
        ConfigSection::TrustedHomes,
        &document.trusted_homes,
        local,
        &deny,
        scope,
    )?;
    let policy = AdmissionPolicy::new(
        mode,
        scope,
        local.clone(),
        allow.iter().cloned().collect(),
        deny,
        dialback.iter().cloned().collect(),
        trusted_homes.iter().cloned().collect(),
    );
    warn_unadmitted(&policy, ConfigSection::DialbackList, &dialback);
    warn_unadmitted(&policy, ConfigSection::TrustedHomes, &trusted_homes);
    Ok(policy)
}

/// An allowlist, dialback or trusted-home list, in file order: exact
/// entries that name no local domain and that no denylist entry covers.
fn exact_list(
    section: ConfigSection,
    entries: &[String],
    local: &BTreeSet<CanonicalDomain>,
    deny: &BTreeSet<CanonicalDomain>,
    scope: PeerScope,
) -> Result<Vec<CanonicalDomain>, ConfigError> {
    let domains = peer_domains(section, entries, scope)?;
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
    Ok(domains)
}

/// Warn about dialback or trusted-home entries that the admission mode
/// never admits, such as every entry in `closed` mode or one missing from
/// the allowlist in `allowlist` mode. Those lists never admit a domain
/// themselves, so such an entry has no effect; it is likely a mistake but
/// not a contradiction, and the snapshot still loads. The warning names
/// the setting and the entry's position, never the domain.
fn warn_unadmitted(policy: &AdmissionPolicy, section: ConfigSection, entries: &[CanonicalDomain]) {
    for (index, domain) in entries.iter().enumerate() {
        if policy.decide(domain).is_err() {
            tracing::warn!(
                setting = section.as_str(),
                index,
                mode = policy.mode().code(),
                "s2s list entry is never admitted in this admission mode"
            );
        }
    }
}

/// Mark the trusted homes that must authenticate under a private-CA
/// profile. Each must be a trusted home and have such a profile.
fn require_private_ca(
    admission: AdmissionPolicy,
    document: &AdmissionDocument,
    trust: &TrustInventory,
    scope: PeerScope,
) -> Result<AdmissionPolicy, ConfigError> {
    let section = ConfigSection::RequirePrivateCa;
    let domains = peer_domains(section, &document.require_private_ca, scope)?;
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

/// A listener or endpoint address: loopback only under [`PeerScope::Test`]
/// (ADR-0020 gate G6). A configured endpoint is the operator's explicit
/// choice of socket for one domain (RFC 6120 §3.2.3), so under
/// [`PeerScope::Public`] it may be any address; DNS-derived addresses are
/// checked by discovery instead.
fn socket_address(
    text: &str,
    section: ConfigSection,
    scope: PeerScope,
) -> Result<SocketAddr, ConfigError> {
    let address: SocketAddr = text
        .parse()
        .map_err(|_| ConfigError::InvalidAddress(section))?;
    if scope == PeerScope::Test && !address.ip().is_loopback() {
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
    scope: PeerScope,
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
        let domains = remote_domains(section, &document.domains, local, scope)?;
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

/// The discovery settings: discovery itself only in the public scope, where
/// remote domains are real DNS names, and the operator's extra blocked
/// address prefixes. Under [`PeerScope::Test`] every admitted domain is
/// within `.test` and every configured endpoint is loopback, so DNS
/// discovery could only ever reach addresses the blocked classes refuse;
/// a configuration that enables it there is a mistake and fails to load.
fn discovery(
    document: &DiscoveryDocument,
    scope: PeerScope,
) -> Result<BlockedAddresses, ConfigError> {
    if document.enabled && scope != PeerScope::Public {
        return Err(ConfigError::DiscoveryRequiresPublicScope);
    }
    if document.blocked_prefixes.len() > MAX_BLOCKED_PREFIXES {
        return Err(ConfigError::TooManyEntries(ConfigSection::BlockedPrefixes));
    }
    let mut blocks: Vec<AddressBlock> = Vec::with_capacity(document.blocked_prefixes.len());
    for (index, entry) in document.blocked_prefixes.iter().enumerate() {
        let block =
            AddressBlock::parse(entry).ok_or(ConfigError::InvalidBlockedPrefix { index })?;
        if blocks.contains(&block) {
            return Err(ConfigError::Duplicate {
                section: ConfigSection::BlockedPrefixes,
                index,
            });
        }
        blocks.push(block);
    }
    Ok(BlockedAddresses::new(blocks))
}

fn endpoints(
    documents: &[EndpointDocument],
    local: &BTreeSet<CanonicalDomain>,
    scope: PeerScope,
) -> Result<BTreeMap<CanonicalDomain, Vec<ConfiguredEndpoint>>, ConfigError> {
    let section = ConfigSection::Endpoints;
    let mut endpoints: BTreeMap<CanonicalDomain, Vec<ConfiguredEndpoint>> = BTreeMap::new();
    for (index, document) in documents.iter().enumerate() {
        let domain = peer_domain(section, index, &document.domain, scope)?;
        if local.contains(&domain) {
            return Err(ConfigError::LocalDomain { section, index });
        }
        let address = socket_address(&document.address, section, scope)?;
        let transport = match document.transport {
            None | Some(EndpointTransportDocument::Starttls) => OutboundTransport::StartTls,
            Some(EndpointTransportDocument::DirectTls) => OutboundTransport::DirectTls,
        };
        if !endpoints.contains_key(&domain) && endpoints.len() >= MAX_ENDPOINT_DOMAINS {
            return Err(ConfigError::TooManyEntries(section));
        }
        let list = endpoints.entry(domain).or_default();
        if list.len() >= MAX_ENDPOINTS_PER_DOMAIN {
            return Err(ConfigError::TooManyEntries(section));
        }
        let endpoint = ConfiguredEndpoint { address, transport };
        if list.iter().any(|existing| existing.address == address) {
            return Err(ConfigError::Duplicate { section, index });
        }
        list.push(endpoint);
    }
    Ok(endpoints)
}

/// A count within `1..=ceiling`, or the default.
fn count(
    name: &'static str,
    value: Option<usize>,
    default: usize,
    ceiling: usize,
) -> Result<usize, ConfigError> {
    match value {
        None => Ok(default),
        Some(value) if value > 0 && value <= ceiling => Ok(value),
        Some(_) => Err(ConfigError::LimitOutOfRange(name)),
    }
}

/// A rate or burst within `floor..=ceiling`, or the default.
fn rate(
    name: &'static str,
    value: Option<u32>,
    default: u32,
    floor: u32,
    ceiling: u32,
) -> Result<u32, ConfigError> {
    match value {
        None => Ok(default),
        Some(value) if value >= floor.max(1) && value <= ceiling => Ok(value),
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
    let limits = resource_limits(document)?;
    check_limits_fit(&limits)?;
    Ok((limits, deadlines(document)?))
}

fn resource_limits(document: &LimitsDocument) -> Result<ResourceLimits, ConfigError> {
    let default = ResourceLimits::DEFAULT;
    let ceiling = ResourceLimits::CEILING;
    let cap = |name, value, pick: fn(&ResourceLimits) -> usize| {
        count(name, value, pick(&default), pick(&ceiling))
    };
    let mut limits = ResourceLimits {
        max_sockets: cap("max_sockets", document.max_sockets, |l| l.max_sockets)?,
        max_inbound_sockets: cap("max_inbound_sockets", document.max_inbound_sockets, |l| {
            l.max_inbound_sockets
        })?,
        max_outbound_sockets: cap("max_outbound_sockets", document.max_outbound_sockets, |l| {
            l.max_outbound_sockets
        })?,
        max_unauthenticated: cap("max_unauthenticated", document.max_unauthenticated, |l| {
            l.max_unauthenticated
        })?,
        max_unauthenticated_per_address: cap(
            "max_unauthenticated_per_address",
            document.max_unauthenticated_per_address,
            |l| l.max_unauthenticated_per_address,
        )?,
        max_inbound_handshakes: cap(
            "max_inbound_handshakes",
            document.max_inbound_handshakes,
            |l| l.max_inbound_handshakes,
        )?,
        max_outbound_handshakes: cap(
            "max_outbound_handshakes",
            document.max_outbound_handshakes,
            |l| l.max_outbound_handshakes,
        )?,
        max_dialing: cap("max_dialing", document.max_dialing, |l| l.max_dialing)?,
        max_backoff_destinations: cap(
            "max_backoff_destinations",
            document.max_backoff_destinations,
            |l| l.max_backoff_destinations,
        )?,
        max_streams: cap("max_streams", document.max_streams, |l| l.max_streams)?,
        max_peers: cap("max_peers", document.max_peers, |l| l.max_peers)?,
        max_negotiating_per_peer: cap(
            "max_negotiating_per_peer",
            document.max_negotiating_per_peer,
            |l| l.max_negotiating_per_peer,
        )?,
        max_verifications: cap("max_verifications", document.max_verifications, |l| {
            l.max_verifications
        })?,
        max_pair_queues: cap("max_pair_queues", document.max_pair_queues, |l| {
            l.max_pair_queues
        })?,
        ..default
    };
    let connections = |name, value, default, ceiling| rate(name, value, default, 1, ceiling);
    limits.accept_rate.per_second = connections(
        "accept_per_second",
        document.accept_per_second,
        default.accept_rate.per_second,
        ceiling.accept_rate.per_second,
    )?;
    limits.accept_rate.burst = connections(
        "accept_burst",
        document.accept_burst,
        default.accept_rate.burst,
        ceiling.accept_rate.burst,
    )?;
    limits.accept_rate_per_address.per_second = connections(
        "accept_per_address_per_second",
        document.accept_per_address_per_second,
        default.accept_rate_per_address.per_second,
        ceiling.accept_rate_per_address.per_second,
    )?;
    limits.accept_rate_per_address.burst = connections(
        "accept_per_address_burst",
        document.accept_per_address_burst,
        default.accept_rate_per_address.burst,
        ceiling.accept_rate_per_address.burst,
    )?;
    limits.dial_rate.per_second = connections(
        "dial_per_second",
        document.dial_per_second,
        default.dial_rate.per_second,
        ceiling.dial_rate.per_second,
    )?;
    limits.dial_rate.burst = connections(
        "dial_burst",
        document.dial_burst,
        default.dial_rate.burst,
        ceiling.dial_rate.burst,
    )?;
    limits.verification_rate.per_second = connections(
        "verification_per_second",
        document.verification_per_second,
        default.verification_rate.per_second,
        ceiling.verification_rate.per_second,
    )?;
    limits.verification_rate.burst = connections(
        "verification_burst",
        document.verification_burst,
        default.verification_rate.burst,
        ceiling.verification_rate.burst,
    )?;
    limits.input_rate_global.per_second = rate(
        "input_per_second",
        document.input_per_second,
        default.input_rate_global.per_second,
        MIN_INPUT_BYTES_PER_SECOND,
        ceiling.input_rate_global.per_second,
    )?;
    limits.input_rate_global.burst = rate(
        "input_burst",
        document.input_burst,
        default.input_rate_global.burst,
        1,
        ceiling.input_rate_global.burst,
    )?;
    limits.input_rate_per_socket.per_second = rate(
        "input_per_socket_per_second",
        document.input_per_socket_per_second,
        default.input_rate_per_socket.per_second,
        MIN_INPUT_BYTES_PER_SECOND,
        ceiling.input_rate_per_socket.per_second,
    )?;
    limits.input_rate_per_socket.burst = rate(
        "input_per_socket_burst",
        document.input_per_socket_burst,
        default.input_rate_per_socket.burst,
        1,
        ceiling.input_rate_per_socket.burst,
    )?;
    Ok(limits)
}

/// Refuse limits that are each within range but do not fit together.
fn check_limits_fit(limits: &ResourceLimits) -> Result<(), ConfigError> {
    // Established streams never take the sockets unauthenticated
    // connections need to negotiate.
    if limits.max_streams + limits.max_unauthenticated > limits.max_sockets {
        return Err(ConfigError::InconsistentLimit("max_streams"));
    }
    // Every peer could hold an inbound stream with an inbound socket to
    // spare for a new connection.
    if limits.max_peers >= limits.max_inbound_sockets {
        return Err(ConfigError::InconsistentLimit("max_peers"));
    }
    // One socket can never be granted more than every socket together.
    if limits.input_rate_global.per_second < limits.input_rate_per_socket.per_second {
        return Err(ConfigError::InconsistentLimit("input_per_second"));
    }
    if limits.input_rate_global.burst < limits.input_rate_per_socket.burst {
        return Err(ConfigError::InconsistentLimit("input_burst"));
    }
    Ok(())
}

fn deadlines(document: &LimitsDocument) -> Result<Deadlines, ConfigError> {
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
        dialback: duration(
            "dialback_timeout_ms",
            document.dialback_timeout_ms,
            defaults.dialback,
            ceilings.dialback,
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
    Ok(deadlines)
}

/// The Server Dialback roles. The Initiating and Authoritative roles need
/// the deployment's secret, read from `secret_file` with one trailing line
/// ending removed and at least [`MIN_DIALBACK_SECRET_BYTES`] long. The
/// Receiving role needs no secret. A secret without the roles that use it,
/// a Receiving role with an empty dialback list, or a dialback list the
/// Receiving role never consults is likely a mistake and logs a warning,
/// but loads.
async fn dialback(
    document: &DialbackDocument,
    admission: &AdmissionPolicy,
    reader: &mut MaterialReader,
) -> Result<DialbackSettings, ConfigError> {
    let section = ConfigSection::Dialback;
    let secret = match (&document.secret_file, document.outbound) {
        (Some(path), true) => {
            let bytes = reader.read(path, section).await?;
            let secret = trim_line_ending(&bytes);
            if secret.len() < MIN_DIALBACK_SECRET_BYTES {
                return Err(ConfigError::DialbackSecretTooShort(
                    MIN_DIALBACK_SECRET_BYTES,
                ));
            }
            Some(Arc::new(DialbackSecret::new(secret)))
        }
        (None, true) => return Err(ConfigError::DialbackSecretMissing),
        (Some(_), false) => {
            tracing::warn!(
                setting = "dialback.secret_file",
                "s2s dialback secret is configured but dialback.outbound is off"
            );
            None
        }
        (None, false) => None,
    };
    let listed = !admission.dialback_list().is_empty();
    if document.inbound && !listed {
        tracing::warn!(
            setting = "admission.dialback",
            "s2s inbound dialback is on but the dialback list is empty"
        );
    }
    if listed && !document.inbound {
        tracing::warn!(
            setting = "dialback.inbound",
            "s2s dialback list is configured but inbound dialback is off"
        );
    }
    Ok(DialbackSettings {
        secret,
        inbound: document.inbound,
    })
}

/// `bytes` without one trailing `\n` or `\r\n`.
fn trim_line_ending(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}
