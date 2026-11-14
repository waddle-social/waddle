//! In-memory TLS handshakes between an initiating and a receiving S2S
//! endpoint. No socket is opened and the clock is fixed.

use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::time_provider::TimeProvider;
use rustls::{
    ClientConfig, ClientConnection, Connection, RootCertStore, ServerConfig, ServerConnection,
    SupportedProtocolVersion,
};
use waddle_server::s2s::verify::{
    initiating_config, receiving_config, OutboundTransport, PeerVerifier, PrivateCaMaterial,
    PrivateCaProfile, PublicRoots, TlsConfigError, TlsSettings, TrustInventory, TrustProfileKind,
    Verification, VerifiedPeer, VerifyError, TLS_PROTOCOL_VERSIONS,
};
use waddle_xmpp::identity::CanonicalDomain;

use super::pki::{unix, Authority, Credential, Day, NOW};

pub const HOME_A: &str = "home-a.test";
pub const HOME_B: &str = "home-b.test";
/// A component domain of [`HOME_A`].
pub const MUC_A: &str = "muc.home-a.test";
/// A domain two labels below [`HOME_A`].
pub const DEEP_A: &str = "a.muc.home-a.test";
/// A well-behaved third domain.
pub const PEER: &str = "peer.test";
/// An internationalized domain in A-label form; its U-label form is
/// `bücher.test`.
pub const IDN_A_LABEL: &str = "xn--bcher-kva.test";
/// The U-label form of [`IDN_A_LABEL`].
pub const IDN_U_LABEL: &str = "bücher.test";
/// The domains a [`Fixture`] accepts its issuer's certificates for.
pub const FIXTURE_DOMAINS: &[&str] = &[HOME_A, HOME_B, MUC_A, DEEP_A, PEER, IDN_A_LABEL];

/// A clock that always tells the same time.
#[derive(Debug)]
struct FixedTime(UnixTime);

impl TimeProvider for FixedTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(self.0)
    }
}

/// TLS settings with the ring provider and the clock fixed at `day`.
pub fn settings(day: Day) -> TlsSettings {
    TlsSettings::new(Arc::new(default_provider()))
        .with_time_provider(Arc::new(FixedTime(unix(day))))
}

/// The canonical form of a domain written in a test.
pub fn domain(text: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(text).expect("a canonical test domain")
}

/// A private-CA profile with `anchors`, `intermediates` and one clean CRL
/// for each of them, loaded at [`NOW`].
pub fn private_profile(anchors: &[&Authority], intermediates: &[&Authority]) -> PrivateCaProfile {
    let material = PrivateCaMaterial {
        anchors: anchors.iter().map(|authority| authority.der()).collect(),
        intermediates: intermediates
            .iter()
            .map(|authority| authority.der())
            .collect(),
        crls: anchors
            .iter()
            .chain(intermediates)
            .map(|authority| authority.clean_crl())
            .collect(),
    };
    PrivateCaProfile::load(&material, &settings(NOW).algorithms(), unix(NOW))
        .expect("a valid private-CA profile")
}

/// An inventory whose public roots are `roots`: the substituted root set
/// that stands in for the public WebPKI.
pub fn public_inventory(roots: &[&Authority]) -> TrustInventory {
    let certificates: Vec<CertificateDer<'static>> =
        roots.iter().map(|authority| authority.der()).collect();
    TrustInventory::new(PublicRoots::from_certificates(&certificates).expect("public roots"))
}

/// An inventory in which `domains` use `profile` and every other domain
/// uses the substituted public roots `public`.
pub fn private_inventory(
    public: &[&Authority],
    profile: PrivateCaProfile,
    domains: &[&str],
) -> TrustInventory {
    let mut inventory = public_inventory(public);
    let profile = Arc::new(profile);
    for name in domains {
        inventory
            .configure_private_ca(domain(name), Arc::clone(&profile))
            .expect("configure a private-CA domain");
    }
    inventory
}

/// The state of both endpoints after a handshake attempt.
pub struct Handshake {
    pub client: Connection,
    pub server: Connection,
    /// The first error the initiating endpoint hit, if any.
    pub client_error: Option<rustls::Error>,
    /// The first error the receiving endpoint hit, if any.
    pub server_error: Option<rustls::Error>,
}

impl Handshake {
    /// Whether both endpoints finished the handshake without an error.
    pub fn completed(&self) -> bool {
        self.client_error.is_none()
            && self.server_error.is_none()
            && !self.client.is_handshaking()
            && !self.server.is_handshaking()
    }
}

/// Move every pending TLS record from `from` to `to` and let `to` process
/// it. An endpoint that has failed still sends its alert but receives
/// nothing more.
fn deliver(
    from: &mut Connection,
    to: &mut Connection,
    to_error: &mut Option<rustls::Error>,
) -> usize {
    let mut moved = 0;
    while from.wants_write() {
        let mut records = Vec::new();
        let written = from
            .write_tls(&mut records)
            .expect("write TLS records to memory");
        if written == 0 {
            break;
        }
        moved += written;
        let mut unread = &records[..];
        // rustls buffers a bounded amount of unprocessed input, so a long
        // flight is read and processed piece by piece. The bound only stops
        // a defect from looping.
        for _ in 0..1024 {
            if unread.is_empty() || to_error.is_some() {
                break;
            }
            // A full input buffer is not a failure: processing below makes
            // room.
            let _ = to.read_tls(&mut unread);
            if let Err(error) = to.process_new_packets() {
                *to_error = Some(error);
            }
        }
    }
    moved
}

/// Run a handshake between `client` and `server` to completion or failure.
pub fn drive(client: ClientConnection, server: ServerConnection) -> Handshake {
    let mut handshake = Handshake {
        client: Connection::Client(client),
        server: Connection::Server(server),
        client_error: None,
        server_error: None,
    };
    // A handshake takes a handful of flights; the bound only stops a
    // defect from looping.
    for _ in 0..16 {
        let to_server = deliver(
            &mut handshake.client,
            &mut handshake.server,
            &mut handshake.server_error,
        );
        let to_client = deliver(
            &mut handshake.server,
            &mut handshake.client,
            &mut handshake.client_error,
        );
        if to_server == 0 && to_client == 0 {
            break;
        }
    }
    handshake
}

/// One S2S stream attempt: the handshake and what each endpoint's verifier
/// established.
pub struct Attempt {
    pub handshake: Handshake,
    /// The initiating endpoint's verifier, which verifies the receiving
    /// server.
    pub initiating: Arc<PeerVerifier>,
    /// The receiving endpoint's verifier, which verifies the initiating
    /// server.
    pub receiving: Arc<PeerVerifier>,
}

impl Attempt {
    /// What the initiating endpoint learned about the receiving server.
    pub fn of_receiver(&self) -> Verification {
        self.initiating.verification()
    }

    /// What the receiving endpoint learned about the initiating server.
    pub fn of_initiator(&self) -> Verification {
        self.receiving.verification()
    }

    /// The evidence about the receiving server that the initiating
    /// endpoint's completed connection carries.
    pub fn receiver_established(&self) -> Option<VerifiedPeer> {
        self.initiating.established(&self.handshake.client)
    }

    /// The evidence about the initiating server that the receiving
    /// endpoint's completed connection carries.
    pub fn initiator_established(&self) -> Option<VerifiedPeer> {
        self.receiving.established(&self.handshake.server)
    }
}

/// A verifier for the peer that must prove `name`.
pub fn verifier(inventory: &TrustInventory, name: &str, day: Day) -> Arc<PeerVerifier> {
    Arc::new(
        PeerVerifier::new(inventory, domain(name), settings(day).algorithms())
            .expect("a verifier for a peer domain"),
    )
}

/// Attempt a stream from `initiator` to `receiver` at `day`. Each endpoint
/// is a domain and the credential it presents; both use `inventory` and the
/// S2S TLS profile.
///
/// The S2S profile refuses to present a certificate it cannot parse. Such a
/// credential is presented through a plain rustls configuration instead, as
/// a misbehaving peer would present it; the endpoint that verifies it still
/// uses the S2S profile.
pub fn attempt_at(
    inventory: &TrustInventory,
    initiator: (&str, &Credential),
    receiver: (&str, &Credential),
    day: Day,
) -> Attempt {
    let settings = settings(day);
    let initiating = verifier(inventory, receiver.0, day);
    let receiving = verifier(inventory, initiator.0, day);
    let client_config = match initiating_config(
        &settings,
        initiator.1.certified_key(),
        Arc::clone(&initiating),
        OutboundTransport::StartTls,
    ) {
        Ok(config) => config,
        Err(TlsConfigError::Credentials(_)) => raw_client_config(
            day,
            TLS_PROTOCOL_VERSIONS,
            Arc::clone(&initiating),
            initiator.1,
            initiator.1,
        ),
        Err(error) => panic!("an initiating configuration: {error}"),
    };
    let server_config = match receiving_config(
        &settings,
        receiver.1.certified_key(),
        Arc::clone(&receiving),
    ) {
        Ok(config) => config,
        Err(TlsConfigError::Credentials(_)) => raw_server_config(
            day,
            TLS_PROTOCOL_VERSIONS,
            Arc::clone(&receiving),
            receiver.1,
            receiver.1,
        ),
        Err(error) => panic!("a receiving configuration: {error}"),
    };
    let handshake = connect(
        client_config,
        server_config,
        initiating.reference().server_name(),
    );
    Attempt {
        handshake,
        initiating,
        receiving,
    }
}

/// [`attempt_at`] at [`NOW`].
pub fn attempt(
    inventory: &TrustInventory,
    initiator: (&str, &Credential),
    receiver: (&str, &Credential),
) -> Attempt {
    attempt_at(inventory, initiator, receiver, NOW)
}

/// Run a handshake between two configurations.
pub fn connect(
    client_config: ClientConfig,
    server_config: ServerConfig,
    server_name: rustls::pki_types::ServerName<'static>,
) -> Handshake {
    let client = ClientConnection::new(Arc::new(client_config), server_name)
        .expect("an initiating connection");
    let server = ServerConnection::new(Arc::new(server_config)).expect("a receiving connection");
    drive(client, server)
}

/// The evidence of a verification that must have succeeded.
pub fn verified(verification: Verification) -> VerifiedPeer {
    match verification {
        Verification::Verified(peer) => peer,
        other => panic!("expected a verified peer, got {other:?}"),
    }
}

/// The failure of a verification that must have been refused.
pub fn refused(verification: Verification) -> VerifyError {
    match verification {
        Verification::Failed(failure) => failure,
        other => panic!("expected a refused certificate, got {other:?}"),
    }
}

/// An initiating configuration built without the S2S profile's credential
/// check, as a misbehaving peer would: `verifier` verifies the server and
/// the client presents `chain` while signing with `signer`'s key.
pub fn raw_client_config(
    day: Day,
    versions: &[&'static SupportedProtocolVersion],
    verifier: Arc<PeerVerifier>,
    chain: &Credential,
    signer: &Credential,
) -> ClientConfig {
    let forged = rustls::sign::CertifiedKey::new(chain.chain.clone(), signer.signing_key());
    ClientConfig::builder_with_details(Arc::new(default_provider()), fixed_clock(day))
        .with_protocol_versions(versions)
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(forged)))
}

/// The receiving counterpart of [`raw_client_config`].
pub fn raw_server_config(
    day: Day,
    versions: &[&'static SupportedProtocolVersion],
    verifier: Arc<PeerVerifier>,
    chain: &Credential,
    signer: &Credential,
) -> ServerConfig {
    let forged = rustls::sign::CertifiedKey::new(chain.chain.clone(), signer.signing_key());
    ServerConfig::builder_with_details(Arc::new(default_provider()), fixed_clock(day))
        .with_protocol_versions(versions)
        .expect("protocol versions")
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(forged)))
}

fn fixed_clock(day: Day) -> Arc<dyn TimeProvider> {
    Arc::new(FixedTime(unix(day)))
}

fn root_store(root: &Authority) -> Arc<RootCertStore> {
    let mut store = RootCertStore::empty();
    store.add(root.der()).expect("add a test root");
    Arc::new(store)
}

/// A stock rustls client that trusts `root`, presents `credential` and
/// keeps sessions for resumption, as an ordinary peer would.
pub fn resuming_client_config(
    root: &Authority,
    credential: &Credential,
    versions: &[&'static SupportedProtocolVersion],
) -> ClientConfig {
    let verifier =
        WebPkiServerVerifier::builder_with_provider(root_store(root), Arc::new(default_provider()))
            .build()
            .expect("a stock server verifier");
    ClientConfig::builder_with_details(Arc::new(default_provider()), fixed_clock(NOW))
        .with_protocol_versions(versions)
        .expect("protocol versions")
        .with_webpki_verifier(verifier)
        .with_client_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(
            credential.certified_key(),
        )))
}

/// A stock rustls server that trusts `root`, presents `credential`, stores
/// sessions and issues tickets, as an ordinary peer would.
pub fn resuming_server_config(root: &Authority, credential: &Credential) -> ServerConfig {
    let verifier =
        WebPkiClientVerifier::builder_with_provider(root_store(root), Arc::new(default_provider()))
            .build()
            .expect("a stock client verifier");
    ServerConfig::builder_with_details(Arc::new(default_provider()), fixed_clock(NOW))
        .with_protocol_versions(TLS_PROTOCOL_VERSIONS)
        .expect("protocol versions")
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(
            credential.certified_key(),
        )))
}

/// A stock rustls client that presents no certificate.
pub fn anonymous_client_config(verifier: Arc<PeerVerifier>) -> ClientConfig {
    ClientConfig::builder_with_details(Arc::new(default_provider()), fixed_clock(NOW))
        .with_protocol_versions(TLS_PROTOCOL_VERSIONS)
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth()
}

/// A CA and an inventory that accepts its certificates.
pub struct Fixture {
    /// The CA whose certificates the inventory accepts for
    /// [`FIXTURE_DOMAINS`].
    pub issuer: Authority,
    pub inventory: TrustInventory,
    pub kind: TrustProfileKind,
}

impl Fixture {
    /// [`FIXTURE_DOMAINS`] use a private-CA profile whose only anchor is
    /// the issuer; every other domain uses unrelated public roots.
    pub fn private() -> Self {
        let issuer = Authority::root("Private Test Root");
        let public = Authority::root("Unrelated Public Root");
        let inventory = private_inventory(
            &[&public],
            private_profile(&[&issuer], &[]),
            FIXTURE_DOMAINS,
        );
        Self {
            issuer,
            inventory,
            kind: TrustProfileKind::PrivateCa,
        }
    }

    /// Every domain uses the public-WebPKI profile, whose substituted root
    /// set holds only the issuer.
    pub fn public() -> Self {
        let issuer = Authority::root("Substituted Public Root");
        let inventory = public_inventory(&[&issuer]);
        Self {
            issuer,
            inventory,
            kind: TrustProfileKind::PublicWebPki,
        }
    }

    /// Both fixtures, for cases that hold in every trust profile.
    pub fn both() -> [Self; 2] {
        [Self::private(), Self::public()]
    }

    /// What is established about `reference` when the credential `issue`
    /// returns is presented in each TLS role: first by the receiving server
    /// of a stream to `reference`, then by the initiating server of a
    /// stream from `reference`. The other endpoint is [`PEER`] with a
    /// certificate the inventory accepts.
    pub fn in_both_roles(
        &self,
        reference: &str,
        issue: impl Fn(&Authority) -> Credential,
    ) -> [Verification; 2] {
        let peer = self.issuer.issue_for(&[PEER]);
        let presented = issue(&self.issuer);
        let as_receiver = attempt(&self.inventory, (PEER, &peer), (reference, &presented));
        let presented = issue(&self.issuer);
        let as_initiator = attempt(&self.inventory, (reference, &presented), (PEER, &peer));
        [as_receiver.of_receiver(), as_initiator.of_initiator()]
    }

    /// The evidence about `reference` that completed connections carry when
    /// the credential `issue` returns is presented in each TLS role, as in
    /// [`Fixture::in_both_roles`]. Both handshakes must complete.
    pub fn established_in_both_roles(
        &self,
        reference: &str,
        issue: impl Fn(&Authority) -> Credential,
    ) -> [VerifiedPeer; 2] {
        let peer = self.issuer.issue_for(&[PEER]);
        let presented = issue(&self.issuer);
        let as_receiver = attempt(&self.inventory, (PEER, &peer), (reference, &presented));
        let presented = issue(&self.issuer);
        let as_initiator = attempt(&self.inventory, (reference, &presented), (PEER, &peer));
        [
            completed(&as_receiver, self.kind).receiver_established(),
            completed(&as_initiator, self.kind).initiator_established(),
        ]
        .map(|evidence| evidence.expect("a completed handshake establishes the peer"))
    }
}

/// `attempt`, which must have completed.
fn completed(attempt: &Attempt, kind: TrustProfileKind) -> &Attempt {
    assert!(
        attempt.handshake.completed(),
        "{kind:?} profile: client {:?}, server {:?}, receiver {:?}, initiator {:?}",
        attempt.handshake.client_error,
        attempt.handshake.server_error,
        attempt.of_receiver(),
        attempt.of_initiator(),
    );
    attempt
}
