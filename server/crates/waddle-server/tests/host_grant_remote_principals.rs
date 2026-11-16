//! Remote principals at the host-grant effect boundary (ADR-0020 D1, D2 and
//! D8; gate G17).
//!
//! A managed room honours a remote principal's grant as its admission
//! witness says, evaluated at each decision from the S2S snapshot the
//! replica runs: nothing while the principal's domain is not admitted (a
//! denylisted parent domain, an allowlist without it, `closed` mode, S2S
//! off), nothing on a stream weaker than host policy requires, and an owner
//! or admin grant only for a trusted home on a `pkix` stream, on a
//! private-CA profile where the operator requires one for that home. A grant
//! that is not honoured contributes nothing, member-level access included.
//! The decisions are the ones room-manifest acceptance, the surfaces the
//! host-grant gate serves and a mutation's initiator make; the trusted homes
//! issuance accepts come from the same snapshot.
//!
//! No inbound S2S stream reaches these surfaces yet, so the stream a
//! request arrived on is given as its evidence (strength and trust-profile
//! kind) instead of a live stream; live streams are the verification
//! phase's two-deployment and third-party cases. Every snapshot is loaded
//! through the production S2S configuration loader from a file, with a
//! fixed clock for the private-CA profile's certificate and CRL.
//!
//! The cases with a database need PostgreSQL only; the fixture activates
//! each scope in place of a verified projection.
#![cfg(feature = "test-support")]

// The shared host-grant and ingress fixtures serve several suites; this one
// uses a subset of each.
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use host_grant_support::{
    bare, issuance_policy, room_submission, sealed_manifest, served, subject, GrantFixture, OWNER,
    PEER, PEER_RESOURCE, SENDER,
};
use rcgen::{
    date_time_ymd, BasicConstraints, CertificateParams, CertificateRevocationListParams,
    DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyIdMethod, KeyPair,
    KeyUsagePurpose, SerialNumber,
};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::UnixTime;
use rustls::time_provider::TimeProvider;
use tempfile::TempDir;
use uuid::Uuid;
use waddle_server::ingress::commit::commit_submission;
use waddle_server::ingress::IngressDecisionClass;
use waddle_server::permissions::host_grant::{
    affiliation_allows, Activation, AdmissionDenial, EnrolledScopes, FederationSnapshot,
    HostGrantAction, HostGrantAttachment, HostGrantError, HostGrantGate, HostGrantStore,
    IssuanceBasis, IssuancePolicy, IssuanceRefusal, MutationReceipt, PrincipalEvidence,
    PrincipalPolicy, PrincipalWitness, RemoteRefusal, RoomAcceptance, RoomAdmission,
    RoomAffiliation, RoomBinding, RoomGate, StreamEvidence,
};
use waddle_server::s2s::config::{AdmissionRefusal, S2sConfig};
use waddle_server::s2s::conn::AuthenticationStrength;
use waddle_server::s2s::verify::{TlsSettings, TrustProfileKind};
use waddle_xmpp::identity::CanonicalDomain;

/// A trusted home in every snapshot that lists one.
const TRUSTED_HOME: &str = "trusted.example";
/// An account of the trusted home holding an admin grant.
const TRUSTED_ADMIN: &str = "admin@trusted.example";
/// An account of an admitted home that is not a trusted home, holding a
/// member grant.
const REMOTE_MEMBER: &str = "guest@remote.example";
/// An account of an admitted home that is not a trusted home, whose admin
/// grant was written outside the canonical path.
const UNLISTED_ADMIN: &str = "admin@unlisted.example";
/// An account of a subdomain of the trusted home.
const SUBDOMAIN_ADMIN: &str = "admin@sub.trusted.example";
/// An account of a subdomain of a domain some snapshots denylist.
const DENIED_CHILD_MEMBER: &str = "guest@chat.parent.example";
const SERVED_CAROL: &str = "carol@example.com";

/// The fixed time the local domain's credentials and the private-CA
/// profile's certificate and CRL are checked against: 2030-06-15T00:00:00Z.
const NOW_SECS: u64 = 1_907_712_000;

#[derive(Debug)]
struct FixedTime;

impl TimeProvider for FixedTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(Duration::from_secs(NOW_SECS)))
    }
}

fn domain(text: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(text).expect("canonical domain")
}

/// One S2S configuration snapshot, written as the operator writes it.
#[derive(Clone)]
struct Snapshot {
    mode: &'static str,
    allow: Vec<&'static str>,
    deny: Vec<&'static str>,
    trusted_homes: Vec<&'static str>,
    /// Domains verified under a private-CA profile.
    private_ca: Vec<&'static str>,
    require_private_ca: Vec<&'static str>,
    require_pkix: bool,
}

impl Snapshot {
    /// `allowlist` mode admitting every home these cases name except the
    /// denylisted parent's subdomain, with [`TRUSTED_HOME`] as the only
    /// trusted home, on the public-WebPKI profile.
    fn trusting() -> Self {
        Self {
            mode: "allowlist",
            allow: vec![
                TRUSTED_HOME,
                "remote.example",
                "unlisted.example",
                "sub.trusted.example",
            ],
            deny: Vec::new(),
            trusted_homes: vec![TRUSTED_HOME],
            private_ca: Vec::new(),
            require_private_ca: Vec::new(),
            require_pkix: false,
        }
    }

    /// The same snapshot after the operator removed the trusted home from
    /// the trusted-home list; it stays admitted.
    fn without_trusted_home() -> Self {
        Self {
            trusted_homes: Vec::new(),
            ..Self::trusting()
        }
    }

    fn toml(&self, ca: &Ca) -> String {
        let list = |entries: &[&str]| {
            let quoted: Vec<String> = entries.iter().map(|entry| format!("{entry:?}")).collect();
            format!("[{}]", quoted.join(", "))
        };
        let mut lines = vec![
            format!("generation = \"{}\"", Uuid::new_v4()),
            "local_domains = [\"example.com\"]".to_owned(),
            "peer_scope = \"public\"".to_owned(),
            // Every local domain presents credentials.
            "[[credentials]]".to_owned(),
            "domain = \"example.com\"".to_owned(),
            format!("certificate_chain = {:?}", ca.chain.display().to_string()),
            format!("private_key = {:?}", ca.key.display().to_string()),
        ];
        if !self.private_ca.is_empty() {
            lines.push("[[private_ca]]".to_owned());
            lines.push(format!("domains = {}", list(&self.private_ca)));
            lines.push(format!("anchors = [{:?}]", ca.anchor.display().to_string()));
            lines.push(format!("crls = [{:?}]", ca.crl.display().to_string()));
        }
        lines.push("[admission]".to_owned());
        lines.push(format!("mode = {:?}", self.mode));
        lines.push(format!("allow = {}", list(&self.allow)));
        lines.push(format!("deny = {}", list(&self.deny)));
        lines.push(format!("trusted_homes = {}", list(&self.trusted_homes)));
        lines.push(format!(
            "require_private_ca = {}",
            list(&self.require_private_ca)
        ));
        lines.push("[host_policy]".to_owned());
        lines.push(format!("require_pkix = {}", self.require_pkix));
        lines.join("\n")
    }

    /// Load the snapshot through the production loader and keep what
    /// host-grant decisions read.
    async fn load(&self) -> FederationSnapshot {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let ca = Ca::write(&dir);
        let path = dir.path().join("s2s.toml");
        std::fs::write(&path, self.toml(&ca)).expect("configuration file");
        let settings =
            TlsSettings::new(Arc::new(default_provider())).with_time_provider(Arc::new(FixedTime));
        let config = S2sConfig::load(&path, settings)
            .await
            .expect("the snapshot loads");
        FederationSnapshot::of(&config)
    }

    /// The principal policy of a replica serving `example.com` and running
    /// this snapshot.
    async fn principals(&self) -> PrincipalPolicy {
        PrincipalPolicy::new(served(), Some(self.load().await))
    }
}

/// A test CA's anchor and an empty, current CRL, used as a private-CA
/// profile, and the local domain's certificate chain and key it issued, as
/// PEM files.
struct Ca {
    anchor: PathBuf,
    crl: PathBuf,
    chain: PathBuf,
    key: PathBuf,
}

impl Ca {
    fn write(dir: &TempDir) -> Self {
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, "Remote principal test CA");
        let mut params = CertificateParams::default();
        params.distinguished_name = name;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        params.not_before = date_time_ymd(2030, 1, 1);
        params.not_after = date_time_ymd(2031, 1, 1);
        params.use_authority_key_identifier_extension = true;
        let key = KeyPair::generate().expect("a CA key");
        let certificate = params.self_signed(&key).expect("a CA certificate");
        let crl = CertificateRevocationListParams {
            this_update: date_time_ymd(2030, 6, 12),
            next_update: date_time_ymd(2030, 6, 19),
            crl_number: SerialNumber::from(1u64),
            issuing_distribution_point: None,
            revoked_certs: Vec::new(),
            key_identifier_method: KeyIdMethod::Sha256,
        }
        .signed_by(&certificate, &key)
        .expect("a CRL");
        let mut leaf = CertificateParams::new(vec!["example.com".to_owned()]).expect("DNS name");
        leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        leaf.not_before = date_time_ymd(2030, 6, 1);
        leaf.not_after = date_time_ymd(2030, 7, 1);
        leaf.use_authority_key_identifier_extension = true;
        let leaf_key = KeyPair::generate().expect("a leaf key");
        let leaf = leaf
            .signed_by(&leaf_key, &certificate, &key)
            .expect("a leaf certificate");
        let write = |file: &str, contents: String| {
            let path = dir.path().join(file);
            std::fs::write(&path, contents).expect("PEM file");
            path
        };
        Self {
            anchor: write("ca.pem", certificate.pem()),
            crl: write("ca.crl.pem", crl.pem().expect("PEM")),
            chain: write("example.com.chain.pem", leaf.pem()),
            key: write("example.com.key.pem", leaf_key.serialize_pem()),
        }
    }
}

/// A request that arrived on a stream authenticated by SASL EXTERNAL after
/// its certificate verified under `profile`.
fn pkix(profile: TrustProfileKind) -> PrincipalEvidence {
    PrincipalEvidence::Stream(StreamEvidence::for_test(
        AuthenticationStrength::Pkix,
        Some(profile),
    ))
}

/// A request that arrived on a stream authenticated by Server Dialback,
/// with the profile its certificate verified under, if it presented one.
fn dialback(profile: Option<TrustProfileKind>) -> PrincipalEvidence {
    PrincipalEvidence::Stream(StreamEvidence::for_test(
        AuthenticationStrength::Dialback,
        profile,
    ))
}

fn public_pkix() -> PrincipalEvidence {
    pkix(TrustProfileKind::PublicWebPki)
}

/// Every way a request can reach a decision that is not a trusted home's
/// `pkix` stream on the public profile.
fn weaker_evidence() -> [PrincipalEvidence; 4] {
    [
        PrincipalEvidence::NoStream,
        dialback(None),
        dialback(Some(TrustProfileKind::PublicWebPki)),
        dialback(Some(TrustProfileKind::PrivateCa)),
    ]
}

/// Whether `account`, reaching the decision with `evidence`, may perform
/// `action` in the fixture's room under `principals`.
async fn admitted(
    fixture: &GrantFixture,
    principals: &PrincipalPolicy,
    account: &str,
    evidence: PrincipalEvidence,
    action: HostGrantAction,
) -> bool {
    fixture
        .store
        .candidate_admission_as(
            &fixture.scope.room,
            &subject(account),
            &evidence,
            action,
            principals,
        )
        .await
        .expect("candidate admission")
        .admitted
}

/// Assert that `account` is admitted to exactly what `honoured` allows.
async fn assert_honoured_as(
    fixture: &GrantFixture,
    principals: &PrincipalPolicy,
    account: &str,
    evidence: PrincipalEvidence,
    honoured: RoomAffiliation,
) {
    for action in HostGrantAction::ALL {
        assert_eq!(
            admitted(fixture, principals, account, evidence, action).await,
            affiliation_allows(honoured, action),
            "{account} {evidence:?} {action:?}"
        );
    }
}

/// Record `affiliation` for `account` through the canonical path under
/// `policy` and activate it.
async fn grant(
    fixture: &GrantFixture,
    account: &str,
    affiliation: RoomAffiliation,
    policy: &IssuancePolicy,
) {
    let command = fixture.command(OWNER, account, affiliation).await;
    fixture
        .store
        .set_affiliation(&command, policy)
        .await
        .expect("grant recorded");
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );
    assert_eq!(
        fixture.affiliations(account).await,
        Some((affiliation, affiliation))
    );
}

/// Record a member grant for `account` and then raise it to `affiliation`
/// in the store directly, as an older binary or an operator tool could,
/// bypassing issuance.
async fn store_outside_the_canonical_path(
    fixture: &GrantFixture,
    account: &str,
    affiliation: RoomAffiliation,
) {
    grant(
        fixture,
        account,
        RoomAffiliation::Member,
        &issuance_policy(&[]),
    )
    .await;
    fixture
        .ingress
        .execute(
            "UPDATE host_grants SET affiliation = ?, effective_affiliation = ? \
             WHERE lifecycle_id = ? AND room_jid = ? AND subject_jid = ?",
            waddle_server::db_params![
                affiliation.as_storage(),
                affiliation.as_storage(),
                fixture.scope.lifecycle.to_storage(),
                fixture.scope.room.as_str(),
                account
            ],
        )
        .await;
    assert_eq!(
        fixture.affiliations(account).await,
        Some((affiliation, affiliation))
    );
}

/// The managed room's grants as the host-grant gate decides them on a
/// replica attached under `principals`.
async fn gate_room(
    fixture: &GrantFixture,
    principals: &PrincipalPolicy,
) -> waddle_server::permissions::host_grant::ManagedRoomGrants {
    let scopes = EnrolledScopes::watch(fixture.store.clone());
    assert!(scopes.wait_loaded(Duration::from_secs(5)).await);
    let gate = HostGrantGate::with_enrolled_scopes(fixture.store.clone(), scopes);
    assert!(gate.attach(
        HostGrantAttachment::unfenced_for_test(&fixture.store, principals)
            .await
            .expect("attachment")
    ));
    match gate.room(RoomBinding::Room(&fixture.scope.room)).await {
        RoomGate::Managed(room) => room,
        other => panic!("the managed room was not decided: {other:?}"),
    }
}

/// Decide a groupchat acceptance from `sender`, reaching it with
/// `evidence`, as room-manifest acceptance does.
async fn decide_acceptance(
    store: &HostGrantStore,
    room: &jid::BareJid,
    principals: &PrincipalPolicy,
    sender: &str,
    evidence: PrincipalEvidence,
    audience: &[jid::BareJid],
) -> RoomAdmission {
    let binding = store.authority_binding().await.expect("authority binding");
    store
        .decide_room_acceptance_for_test(&RoomAcceptance {
            room,
            sender: &bare(sender),
            sender_evidence: evidence,
            action: HostGrantAction::SendMessage,
            audience,
            principals,
            binding: &binding,
        })
        .await
        .expect("acceptance decided")
        .expect("the room is managed")
}

#[tokio::test]
async fn a_stored_privileged_grant_is_honoured_only_for_a_listed_home_on_pkix() {
    let Some(fixture) = GrantFixture::active("hg_w5e_privileged").await else {
        return;
    };
    grant(
        &fixture,
        TRUSTED_ADMIN,
        RoomAffiliation::Admin,
        &issuance_policy(&[TRUSTED_HOME]),
    )
    .await;
    store_outside_the_canonical_path(&fixture, UNLISTED_ADMIN, RoomAffiliation::Admin).await;
    let trusting = Snapshot::trusting().principals().await;
    let removed = Snapshot::without_trusted_home().principals().await;

    // A listed home on a `pkix` stream: the admin grant is honoured.
    assert_honoured_as(
        &fixture,
        &trusting,
        TRUSTED_ADMIN,
        public_pkix(),
        RoomAffiliation::Admin,
    )
    .await;
    // On a dialback-only stream, or with no stream at all, it contributes
    // nothing, member-level access included.
    for evidence in weaker_evidence() {
        assert_honoured_as(
            &fixture,
            &trusting,
            TRUSTED_ADMIN,
            evidence,
            RoomAffiliation::None,
        )
        .await;
    }
    // The home removed from the list: nothing, whatever is stored.
    assert_honoured_as(
        &fixture,
        &removed,
        TRUSTED_ADMIN,
        public_pkix(),
        RoomAffiliation::None,
    )
    .await;
    // A home never listed, whose admin grant was stored outside the
    // canonical path: nothing, on any stream.
    for evidence in [public_pkix(), pkix(TrustProfileKind::PrivateCa)] {
        assert_honoured_as(
            &fixture,
            &trusting,
            UNLISTED_ADMIN,
            evidence,
            RoomAffiliation::None,
        )
        .await;
    }

    // The gate's surfaces decide the same.
    let room = gate_room(&fixture, &trusting).await;
    assert!(room.allows_as(
        &bare(TRUSTED_ADMIN),
        &public_pkix(),
        HostGrantAction::Manage
    ));
    assert_eq!(
        room.standing_as(&bare(TRUSTED_ADMIN), &public_pkix()),
        RoomAffiliation::Admin
    );
    assert!(!room.allows_as(&bare(TRUSTED_ADMIN), &dialback(None), HostGrantAction::Read));
    assert_eq!(
        room.standing_as(&bare(TRUSTED_ADMIN), &dialback(None)),
        RoomAffiliation::None
    );
    // A subject the surface names, rather than one whose request it
    // decides, has no stream evidence: the remote admin is not listed as
    // admitted, nor is it given the moderator role.
    assert!(!room.allows(&bare(TRUSTED_ADMIN), HostGrantAction::Read));
    assert!(!room
        .admitted(HostGrantAction::Read)
        .contains(&bare(TRUSTED_ADMIN)));
    assert!(!room
        .witness(&bare(TRUSTED_ADMIN), &PrincipalEvidence::NoStream)
        .privilege_honoured());
    let removed_room = gate_room(&fixture, &removed).await;
    assert!(!removed_room.allows_as(&bare(TRUSTED_ADMIN), &public_pkix(), HostGrantAction::Read));
    fixture.close().await;
}

#[tokio::test]
async fn room_moderation_by_a_remote_principal_needs_a_trusted_home_on_pkix() {
    let Some(fixture) = GrantFixture::active("hg_w5e_moderation").await else {
        return;
    };
    grant(
        &fixture,
        TRUSTED_ADMIN,
        RoomAffiliation::Admin,
        &issuance_policy(&[TRUSTED_HOME]),
    )
    .await;
    grant(
        &fixture,
        REMOTE_MEMBER,
        RoomAffiliation::Member,
        &issuance_policy(&[]),
    )
    .await;
    // Written last, so no activation of the fixture runs over it.
    store_outside_the_canonical_path(&fixture, UNLISTED_ADMIN, RoomAffiliation::Admin).await;
    let trusting = Snapshot::trusting().principals().await;
    let moderate = HostGrantAction::Moderate;
    // A trusted home on `pkix` moderates; on a dialback-only stream it
    // does not.
    assert!(admitted(&fixture, &trusting, TRUSTED_ADMIN, public_pkix(), moderate).await);
    for evidence in weaker_evidence() {
        assert!(
            !admitted(&fixture, &trusting, TRUSTED_ADMIN, evidence, moderate).await,
            "{evidence:?}"
        );
    }
    // A home that is not on the list does not moderate on `pkix` either.
    assert!(!admitted(&fixture, &trusting, UNLISTED_ADMIN, public_pkix(), moderate).await);

    // A message author's own correction and retraction are not moderation:
    // a remote member edits and deletes its own messages on any stream
    // while its domain is admitted, and never moderates.
    for evidence in [public_pkix(), dialback(None), PrincipalEvidence::NoStream] {
        for action in [
            HostGrantAction::EditOwnMessage,
            HostGrantAction::DeleteOwnMessage,
            HostGrantAction::SendMessage,
        ] {
            assert!(
                admitted(&fixture, &trusting, REMOTE_MEMBER, evidence, action).await,
                "{evidence:?} {action:?}"
            );
        }
        assert!(!admitted(&fixture, &trusting, REMOTE_MEMBER, evidence, moderate).await);
    }
    fixture.close().await;
}

#[tokio::test]
async fn a_former_admin_of_a_removed_home_cannot_moderate_or_read() {
    let Some(fixture) = GrantFixture::active("hg_w5e_former_admin").await else {
        return;
    };
    grant(
        &fixture,
        TRUSTED_ADMIN,
        RoomAffiliation::Admin,
        &issuance_policy(&[TRUSTED_HOME]),
    )
    .await;
    let trusting = Snapshot::trusting().principals().await;
    let removed = Snapshot::without_trusted_home().principals().await;
    let audience = [bare(PEER), bare(TRUSTED_ADMIN)];

    // While the home is listed, the admin's groupchat on a `pkix` stream is
    // admitted.
    assert!(matches!(
        decide_acceptance(
            &fixture.store,
            &fixture.scope.room,
            &trusting,
            TRUSTED_ADMIN,
            public_pkix(),
            &audience,
        )
        .await,
        RoomAdmission::Admitted { .. }
    ));
    // Once the operator removes the home, the same stream's request, queued
    // before the change, is refused: the grant contributes no permission,
    // read and send included.
    assert_eq!(
        decide_acceptance(
            &fixture.store,
            &fixture.scope.room,
            &removed,
            TRUSTED_ADMIN,
            public_pkix(),
            &audience,
        )
        .await,
        RoomAdmission::Denied(AdmissionDenial::SenderNotAdmitted)
    );
    for action in HostGrantAction::ALL {
        assert!(
            !admitted(&fixture, &removed, TRUSTED_ADMIN, public_pkix(), action).await,
            "{action:?}"
        );
    }
    let room = gate_room(&fixture, &removed).await;
    for action in [HostGrantAction::Moderate, HostGrantAction::Read] {
        assert!(!room.allows_as(&bare(TRUSTED_ADMIN), &public_pkix(), action));
    }
    fixture.close().await;
}

#[tokio::test]
async fn the_admission_witness_binds_the_remote_sender() {
    let Some(fixture) = GrantFixture::active("hg_w5e_witness").await else {
        return;
    };
    grant(
        &fixture,
        TRUSTED_ADMIN,
        RoomAffiliation::Admin,
        &issuance_policy(&[TRUSTED_HOME]),
    )
    .await;
    grant(
        &fixture,
        REMOTE_MEMBER,
        RoomAffiliation::Member,
        &issuance_policy(&[]),
    )
    .await;
    let snapshot = Snapshot::trusting().load().await;
    let principals = PrincipalPolicy::new(served(), Some(snapshot.clone()));
    let audience = [
        bare(PEER),
        bare(REMOTE_MEMBER),
        bare(TRUSTED_ADMIN),
        bare(SERVED_CAROL),
    ];
    let RoomAdmission::Admitted { witness, audience } = decide_acceptance(
        &fixture.store,
        &fixture.scope.room,
        &principals,
        TRUSTED_ADMIN,
        public_pkix(),
        &audience,
    )
    .await
    else {
        panic!("the trusted home's admin on a pkix stream is admitted");
    };
    let PrincipalWitness::Remote(sender) = &witness.sender else {
        panic!("the sender is a remote principal");
    };
    assert_eq!(sender.home(), TRUSTED_HOME);
    assert_eq!(sender.strength(), Some(AuthenticationStrength::Pkix));
    assert_eq!(sender.trust_profile(), Some(TrustProfileKind::PublicWebPki));
    assert_eq!(sender.snapshot(), Some(snapshot.generation()));
    assert_eq!(sender.admission(), Ok(()));
    assert!(sender.trusted_home_listed());
    assert!(sender.trusted_home());
    assert!(!sender.pkix_required());
    assert!(sender.privilege_honoured());
    // The named recipients are decided without stream evidence: the served
    // member and the admitted remote member read, the remote admin does
    // not, and an account without a grant does not.
    assert_eq!(audience, vec![bare(PEER), bare(REMOTE_MEMBER)]);

    // A dialback-only stream of the same home: the witness records it, and
    // the admin grant contributes nothing.
    let dialback_witness = principals.witness(&bare(TRUSTED_ADMIN), &dialback(None));
    let remote = dialback_witness.remote().expect("remote witness");
    assert_eq!(remote.strength(), Some(AuthenticationStrength::Dialback));
    assert_eq!(remote.trust_profile(), None);
    assert!(remote.trusted_home_listed());
    assert!(!remote.trusted_home());
    assert!(!remote.privilege_honoured());
    assert_eq!(
        decide_acceptance(
            &fixture.store,
            &fixture.scope.room,
            &principals,
            TRUSTED_ADMIN,
            dialback(None),
            &[],
        )
        .await,
        RoomAdmission::Denied(AdmissionDenial::SenderNotAdmitted)
    );

    // A local sender is decided as a served account, as before.
    let RoomAdmission::Admitted { witness, .. } = decide_acceptance(
        &fixture.store,
        &fixture.scope.room,
        &principals,
        SENDER,
        PrincipalEvidence::NoStream,
        &[],
    )
    .await
    else {
        panic!("the served member is admitted");
    };
    assert_eq!(witness.sender, PrincipalWitness::Served);
    fixture.close().await;
}

#[tokio::test]
async fn work_from_a_domain_no_longer_admitted_is_refused() {
    let Some(fixture) = GrantFixture::active("hg_w5e_admission").await else {
        return;
    };
    for account in [REMOTE_MEMBER, DENIED_CHILD_MEMBER] {
        grant(
            &fixture,
            account,
            RoomAffiliation::Member,
            &issuance_policy(&[]),
        )
        .await;
    }
    let open = Snapshot {
        mode: "open",
        allow: Vec::new(),
        trusted_homes: Vec::new(),
        ..Snapshot::trusting()
    };
    let cases = [
        // Admitted: the member-level grant is honoured on any stream.
        (Snapshot::trusting(), REMOTE_MEMBER, None),
        (open.clone(), DENIED_CHILD_MEMBER, None),
        // Removed from the allowlist.
        (
            Snapshot {
                allow: vec![TRUSTED_HOME],
                ..Snapshot::trusting()
            },
            REMOTE_MEMBER,
            Some(AdmissionRefusal::NotAllowlisted),
        ),
        // A parent domain denylisted, in `open` mode.
        (
            Snapshot {
                deny: vec!["parent.example"],
                ..open.clone()
            },
            DENIED_CHILD_MEMBER,
            Some(AdmissionRefusal::Denylisted),
        ),
        // The deployment switched to `closed`.
        (
            Snapshot {
                mode: "closed",
                allow: Vec::new(),
                trusted_homes: Vec::new(),
                ..Snapshot::trusting()
            },
            REMOTE_MEMBER,
            Some(AdmissionRefusal::Closed),
        ),
    ];
    for (snapshot, account, refusal) in cases {
        let principals = snapshot.principals().await;
        let witness = principals.witness(&bare(account), &public_pkix());
        assert_eq!(
            witness.remote().map(|remote| remote.admission()),
            Some(refusal.map_or(Ok(()), |refusal| Err(RemoteRefusal::Admission(refusal)))),
            "{account}"
        );
        let honoured = if refusal.is_none() {
            RoomAffiliation::Member
        } else {
            RoomAffiliation::None
        };
        for evidence in [public_pkix(), dialback(None), PrincipalEvidence::NoStream] {
            assert_honoured_as(&fixture, &principals, account, evidence, honoured).await;
        }
        let room = gate_room(&fixture, &principals).await;
        assert_eq!(
            room.admitted(HostGrantAction::Read)
                .contains(&bare(account)),
            refusal.is_none(),
            "{account}"
        );
    }

    // S2S off: no remote domain is admitted.
    let local = PrincipalPolicy::local(served());
    assert_eq!(
        local
            .witness(&bare(REMOTE_MEMBER), &public_pkix())
            .remote()
            .map(|remote| remote.admission()),
        Some(Err(RemoteRefusal::S2sDisabled))
    );
    assert_honoured_as(
        &fixture,
        &local,
        REMOTE_MEMBER,
        public_pkix(),
        RoomAffiliation::None,
    )
    .await;
    fixture.close().await;
}

#[tokio::test]
async fn acceptance_reduces_the_audience_to_admitted_remote_members() {
    let principals = Snapshot::trusting().principals().await;
    let Some(fixture) = GrantFixture::active_under("hg_w5e_audience", principals).await else {
        return;
    };
    grant(
        &fixture,
        REMOTE_MEMBER,
        RoomAffiliation::Member,
        &issuance_policy(&[]),
    )
    .await;
    grant(
        &fixture,
        TRUSTED_ADMIN,
        RoomAffiliation::Admin,
        &issuance_policy(&[TRUSTED_HOME]),
    )
    .await;
    // The acceptance names the remote member and the trusted home's admin
    // as seated occupants. The member's domain is admitted, so it is frozen
    // a child; the admin is named without stream evidence, so its grant
    // contributes nothing and it is frozen none.
    let submission = room_submission(
        &fixture.ingress,
        &fixture.scope.room,
        "remote-audience",
        "who reads this?",
        &[
            PEER_RESOURCE,
            "guest@remote.example/phone",
            "admin@trusted.example/desk",
        ],
    );
    let decision = commit_submission(&fixture.ingress.uow, &submission, 1)
        .await
        .expect("accepted for the admitted audience");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    let manifest = sealed_manifest(&fixture.ingress, decision.message_key.expect("key"))
        .await
        .expect("sealed manifest");
    let mut accounts: Vec<String> = manifest
        .children
        .iter()
        .filter_map(|child| {
            child
                .target
                .as_ref()
                .map(|target| target.to_bare().to_string())
        })
        .collect();
    accounts.sort();
    accounts.dedup();
    assert_eq!(
        accounts,
        vec![PEER.to_owned(), REMOTE_MEMBER.to_owned(), SENDER.to_owned()]
    );
    fixture.close().await;
}

#[tokio::test]
async fn host_policy_can_require_pkix_for_every_remote_grant() {
    let Some(fixture) = GrantFixture::active("hg_w5e_require_pkix").await else {
        return;
    };
    grant(
        &fixture,
        REMOTE_MEMBER,
        RoomAffiliation::Member,
        &issuance_policy(&[]),
    )
    .await;
    grant(
        &fixture,
        TRUSTED_ADMIN,
        RoomAffiliation::Admin,
        &issuance_policy(&[TRUSTED_HOME]),
    )
    .await;
    let strict = Snapshot {
        require_pkix: true,
        ..Snapshot::trusting()
    }
    .principals()
    .await;
    // A member-level grant is honoured on `pkix` only; on a dialback stream,
    // or named without a stream, it contributes nothing.
    assert_honoured_as(
        &fixture,
        &strict,
        REMOTE_MEMBER,
        public_pkix(),
        RoomAffiliation::Member,
    )
    .await;
    for evidence in weaker_evidence() {
        assert_honoured_as(
            &fixture,
            &strict,
            REMOTE_MEMBER,
            evidence,
            RoomAffiliation::None,
        )
        .await;
    }
    // A trusted home's privileged grant still needs `pkix`, as without the
    // switch.
    assert_honoured_as(
        &fixture,
        &strict,
        TRUSTED_ADMIN,
        public_pkix(),
        RoomAffiliation::Admin,
    )
    .await;
    let witness = strict.witness(&bare(REMOTE_MEMBER), &dialback(None));
    assert!(witness.remote().expect("remote").pkix_required());
    // Served accounts are not affected by the switch.
    assert_honoured_as(
        &fixture,
        &strict,
        SENDER,
        PrincipalEvidence::NoStream,
        RoomAffiliation::Member,
    )
    .await;
    fixture.close().await;
}

#[tokio::test]
async fn a_trusted_home_that_requires_a_private_ca_counts_only_on_it() {
    let Some(fixture) = GrantFixture::active("hg_w5e_private_ca").await else {
        return;
    };
    grant(
        &fixture,
        TRUSTED_ADMIN,
        RoomAffiliation::Admin,
        &issuance_policy(&[TRUSTED_HOME]),
    )
    .await;
    let private = Snapshot {
        private_ca: vec![TRUSTED_HOME],
        require_private_ca: vec![TRUSTED_HOME],
        ..Snapshot::trusting()
    }
    .principals()
    .await;
    assert_honoured_as(
        &fixture,
        &private,
        TRUSTED_ADMIN,
        pkix(TrustProfileKind::PrivateCa),
        RoomAffiliation::Admin,
    )
    .await;
    // On the public profile the home is not a trusted home: its admin grant
    // contributes nothing, not even read.
    assert_honoured_as(
        &fixture,
        &private,
        TRUSTED_ADMIN,
        public_pkix(),
        RoomAffiliation::None,
    )
    .await;
    let witness = private.witness(&bare(TRUSTED_ADMIN), &public_pkix());
    let remote = witness.remote().expect("remote");
    assert!(remote.trusted_home_listed());
    assert!(!remote.trusted_home());
    // A dialback stream whose certificate verified under the private CA is
    // still dialback.
    assert_honoured_as(
        &fixture,
        &private,
        TRUSTED_ADMIN,
        dialback(Some(TrustProfileKind::PrivateCa)),
        RoomAffiliation::None,
    )
    .await;
    fixture.close().await;
}

#[tokio::test]
async fn a_remote_initiator_acts_only_as_its_witness_honours() {
    let Some(fixture) = GrantFixture::active("hg_w5e_initiator").await else {
        return;
    };
    grant(
        &fixture,
        TRUSTED_ADMIN,
        RoomAffiliation::Admin,
        &issuance_policy(&[TRUSTED_HOME]),
    )
    .await;
    let services = [domain("muc.example.com"), domain("spaces.example.com")];
    let trusting =
        IssuancePolicy::for_principals(&Snapshot::trusting().principals().await, services.clone())
            .expect("issuance policy");
    let removed = IssuancePolicy::for_principals(
        &Snapshot::without_trusted_home().principals().await,
        services,
    )
    .expect("issuance policy");
    let change = |affiliation| fixture.command(TRUSTED_ADMIN, SERVED_CAROL, affiliation);

    // On a weaker stream, without one, or once the home is removed, the
    // admin grant authorizes nothing.
    for evidence in weaker_evidence() {
        let command = change(RoomAffiliation::Member).await;
        assert!(
            matches!(
                fixture
                    .store
                    .set_affiliation_as(&command, &trusting, &evidence)
                    .await,
                Err(HostGrantError::InitiatorNotAuthorized)
            ),
            "{evidence:?}"
        );
    }
    let command = change(RoomAffiliation::Member).await;
    assert!(matches!(
        fixture
            .store
            .set_affiliation_as(&command, &removed, &public_pkix())
            .await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));
    assert_eq!(fixture.affiliations(SERVED_CAROL).await, None);

    // On the trusted home's `pkix` stream the admin changes member
    // standing, and only an owner changes owner or admin standing.
    let command = change(RoomAffiliation::Member).await;
    assert!(matches!(
        fixture
            .store
            .set_affiliation_as(&command, &trusting, &public_pkix())
            .await,
        Ok(MutationReceipt::Recorded(_))
    ));
    assert_eq!(
        fixture
            .affiliations(SERVED_CAROL)
            .await
            .map(|(recorded, _)| recorded),
        Some(RoomAffiliation::Member)
    );
    let command = change(RoomAffiliation::Admin).await;
    assert!(matches!(
        fixture
            .store
            .set_affiliation_as(&command, &trusting, &public_pkix())
            .await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));
    fixture.close().await;
}

#[tokio::test]
async fn issuance_takes_its_trusted_homes_from_the_snapshot() {
    let services = [domain("muc.example.com"), domain("spaces.example.com")];
    let trusting = Snapshot::trusting().principals().await;
    let policy =
        IssuancePolicy::for_principals(&trusting, services.clone()).expect("issuance policy");
    assert_eq!(policy.principals(), &trusting);
    assert_eq!(policy.served_account_domains(), &served());
    assert_eq!(
        policy.check(&subject(TRUSTED_ADMIN), RoomAffiliation::Admin),
        Ok(IssuanceBasis::TrustedHome)
    );
    assert_eq!(
        policy.check(&subject(REMOTE_MEMBER), RoomAffiliation::Admin),
        Err(IssuanceRefusal::UntrustedHome)
    );
    // Entries match exactly: a subdomain of a trusted home is not one.
    assert_eq!(
        policy.check(&subject(SUBDOMAIN_ADMIN), RoomAffiliation::Owner),
        Err(IssuanceRefusal::UntrustedHome)
    );
    assert!(policy.admits_privileged_subject(&bare(TRUSTED_ADMIN)));
    assert!(!policy.admits_privileged_subject(&bare(REMOTE_MEMBER)));
    assert_eq!(
        policy.check(&subject(REMOTE_MEMBER), RoomAffiliation::Member),
        Ok(IssuanceBasis::Unprivileged)
    );

    // While S2S is off no trusted home is listed.
    let local = IssuancePolicy::for_principals(&PrincipalPolicy::local(served()), services.clone())
        .expect("issuance policy");
    assert_eq!(
        local.check(&subject(TRUSTED_ADMIN), RoomAffiliation::Admin),
        Err(IssuanceRefusal::UntrustedHome)
    );

    // A trusted home that is one of this deployment's service domains, which
    // the S2S loader does not know, is refused.
    let upload = Snapshot {
        allow: vec!["upload.example.com"],
        trusted_homes: vec!["upload.example.com"],
        ..Snapshot::trusting()
    }
    .principals()
    .await;
    assert_eq!(
        IssuancePolicy::for_principals(&upload, [domain("upload.example.com")]).err(),
        Some(IssuanceRefusal::TrustedHomeIsLocal)
    );
}

#[tokio::test]
async fn subdomains_and_served_accounts_on_streams_are_not_trusted_homes() {
    let trusting = Snapshot::trusting().principals().await;
    // A subdomain of a trusted home, admitted on its own: not a trusted home.
    let subdomain = trusting.witness(&bare(SUBDOMAIN_ADMIN), &public_pkix());
    let remote = subdomain.remote().expect("remote");
    assert_eq!(remote.admission(), Ok(()));
    assert!(!remote.trusted_home_listed());
    assert!(!remote.privilege_honoured());
    assert_eq!(
        subdomain.honour(RoomAffiliation::Admin),
        RoomAffiliation::None
    );
    assert_eq!(
        subdomain.honour(RoomAffiliation::Member),
        RoomAffiliation::Member
    );

    // A stream never carries an account of a served domain: such a request
    // is refused rather than decided as local.
    let claimed = trusting.witness(&bare(OWNER), &public_pkix());
    assert_eq!(
        claimed.remote().map(|remote| remote.admission()),
        Some(Err(RemoteRefusal::ServedDomainOnStream))
    );
    for affiliation in RoomAffiliation::ALL {
        let expected = if affiliation == RoomAffiliation::Outcast {
            RoomAffiliation::Outcast
        } else {
            RoomAffiliation::None
        };
        assert_eq!(claimed.honour(affiliation), expected);
    }

    // The local path: a served account without a stream is honoured as
    // stored, under every snapshot.
    for principals in [
        trusting.clone(),
        PrincipalPolicy::local(served()),
        Snapshot {
            require_pkix: true,
            ..Snapshot::trusting()
        }
        .principals()
        .await,
    ] {
        let local = principals.witness(&bare(OWNER), &PrincipalEvidence::NoStream);
        assert_eq!(local, PrincipalWitness::Served);
        for affiliation in RoomAffiliation::ALL {
            assert_eq!(local.honour(affiliation), affiliation);
        }
    }
    // A ban of a remote principal is honoured whatever else holds.
    for evidence in weaker_evidence() {
        assert_eq!(
            trusting
                .witness(&bare(TRUSTED_ADMIN), &evidence)
                .honour(RoomAffiliation::Outcast),
            RoomAffiliation::Outcast
        );
    }
}
