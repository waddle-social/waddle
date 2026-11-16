//! Whose grants a decision honours: the effect-boundary rule for remote
//! principals (ADR-0020 D1, D2 and D8; gate G17).
//!
//! Every decision that reads a managed room's grants (room-manifest
//! acceptance, the surfaces [`super::HostGrantGate`] serves and a mutation's
//! initiator) asks [`PrincipalPolicy::witness`] what the principal it
//! decides for is, and honours a stored affiliation as
//! [`PrincipalWitness::honour`] says:
//!
//! - An account of a domain this deployment serves, deciding without an
//!   S2S stream, is honoured as stored.
//! - Any other principal is remote. Its witness binds its home domain, the
//!   authentication strength and trust-profile kind of the inbound S2S
//!   stream that carried its request (none when no stream did), the D1
//!   admission decision for that domain under the S2S snapshot the replica
//!   runs, a denylisted parent domain included, and whether that domain is
//!   a trusted home for that stream (D8), a home whose `require_private_ca`
//!   entry the stream's profile does not meet included. All of it is
//!   evaluated at the decision, from that snapshot: nothing carried with
//!   queued work is trusted for it.
//! - A remote principal's grant contributes nothing unless its domain is
//!   admitted now and, when host policy requires `pkix`, the stream had
//!   `pkix` strength. A privileged or moderator affiliation (owner, admin)
//!   is honoured only for a trusted home on a `pkix` stream; otherwise it
//!   contributes nothing, neither moderation nor the member-level access
//!   the policy table would derive from it. A member-level grant is
//!   honoured on any stream while the domain is admitted. A ban is always
//!   honoured.
//! - While S2S is off the replica runs no snapshot and admits no remote
//!   domain, so no remote principal's grant contributes anything.
//!
//! A principal the decision names without its request (a recipient of a
//! groupchat, a seated occupant, a listed subject) has no stream evidence
//! here, so a remote one holds no privileged authority or room moderation,
//! and no member-level access either when host policy requires `pkix`.
//!
//! The snapshot is immutable for the life of the process: a replica applies
//! a changed one when it restarts with it, and the witness names the
//! snapshot it was decided under.

use std::sync::Arc;

use jid::BareJid;
use uuid::Uuid;
use waddle_xmpp::identity::CanonicalDomain;

use super::issuance::{ServedAccountDomains, SubjectHome};
use super::model::RoomAffiliation;
use crate::s2s::config::{AdmissionPolicy, AdmissionRefusal, S2sConfig};
use crate::s2s::conn::AuthenticationStrength;
use crate::s2s::route::RemotePrincipal;
use crate::s2s::verify::TrustProfileKind;

/// The S2S configuration a replica runs, as decisions about remote
/// principals read it: the snapshot's identifier, its admission policy with
/// the trusted-home list, and whether host policy requires `pkix` strength
/// for any grant of a remote principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FederationSnapshot {
    generation: Uuid,
    admission: Arc<AdmissionPolicy>,
    require_pkix: bool,
    federate_new_rooms: bool,
}

impl FederationSnapshot {
    /// The parts of `config` decisions about remote principals read.
    pub fn of(config: &S2sConfig) -> Self {
        Self {
            generation: config.generation(),
            admission: Arc::clone(config.admission()),
            require_pkix: config.grants_require_pkix(),
            federate_new_rooms: config.federates_new_rooms(),
        }
    }

    /// Whether a room created now starts with its `federated` setting on.
    pub fn federates_new_rooms(&self) -> bool {
        self.federate_new_rooms
    }

    /// The identifier of the snapshot.
    pub fn generation(&self) -> Uuid {
        self.generation
    }

    /// The admission policy, with the trusted-home list.
    pub fn admission(&self) -> &AdmissionPolicy {
        &self.admission
    }

    /// Whether a remote principal's grants are honoured only on a stream
    /// with `pkix` strength, member-level grants included.
    pub fn requires_pkix(&self) -> bool {
        self.require_pkix
    }
}

/// The deployment's inputs to every decision about whose grants are
/// honoured: the account domains it serves and the S2S snapshot it runs,
/// which is absent while S2S is off.
///
/// One value feeds admission, the initiator of a mutation and issuance
/// ([`super::IssuancePolicy::for_principals`]), so the served account
/// domains and the trusted-home list each come from one configuration
/// value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrincipalPolicy {
    served: ServedAccountDomains,
    federation: Option<FederationSnapshot>,
}

impl PrincipalPolicy {
    /// The policy of a replica that serves `served` and runs `federation`,
    /// or no S2S snapshot at all.
    pub fn new(served: ServedAccountDomains, federation: Option<FederationSnapshot>) -> Self {
        Self { served, federation }
    }

    /// The policy of a replica that serves `served` with S2S off: every
    /// remote principal's grant contributes nothing.
    pub fn local(served: ServedAccountDomains) -> Self {
        Self::new(served, None)
    }

    /// The account domains this deployment serves.
    pub fn served(&self) -> &ServedAccountDomains {
        &self.served
    }

    /// The S2S snapshot the replica runs; `None` while S2S is off.
    pub fn federation(&self) -> Option<&FederationSnapshot> {
        self.federation.as_ref()
    }

    /// The operator's trusted-home list of the snapshot; empty while S2S is
    /// off.
    pub fn trusted_homes(&self) -> Vec<CanonicalDomain> {
        self.federation
            .as_ref()
            .map(|federation| {
                federation
                    .admission
                    .trusted_homes()
                    .iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What `subject` is to a decision it reaches with `evidence`,
    /// evaluated now from this policy.
    pub fn witness(&self, subject: &BareJid, evidence: &PrincipalEvidence) -> PrincipalWitness {
        let stream = match evidence {
            PrincipalEvidence::NoStream => None,
            PrincipalEvidence::Stream(stream) => Some(*stream),
        };
        let served = self.served.home_of(subject) == SubjectHome::Served;
        if served && stream.is_none() {
            return PrincipalWitness::Served;
        }
        let mut witness = RemoteWitness {
            home: subject.domain().as_str().to_owned(),
            strength: stream.map(|stream| stream.strength),
            trust_profile: stream.and_then(|stream| stream.trust_profile),
            snapshot: self.federation.as_ref().map(FederationSnapshot::generation),
            admission: Ok(()),
            trusted_home_listed: false,
            trusted_home: false,
            pkix_required: self
                .federation
                .as_ref()
                .is_some_and(FederationSnapshot::requires_pkix),
        };
        // A stream never carries an account of a domain this deployment
        // serves: such a principal is refused rather than decided as local.
        if served {
            witness.admission = Err(RemoteRefusal::ServedDomainOnStream);
            return PrincipalWitness::Remote(witness);
        }
        let Some(federation) = &self.federation else {
            witness.admission = Err(RemoteRefusal::S2sDisabled);
            return PrincipalWitness::Remote(witness);
        };
        let Ok(home) = CanonicalDomain::parse_canonical(subject.domain().as_str()) else {
            witness.admission = Err(RemoteRefusal::NotCanonical);
            return PrincipalWitness::Remote(witness);
        };
        let admission = federation.admission();
        witness.admission = admission.decide(&home).map_err(RemoteRefusal::Admission);
        witness.trusted_home_listed = admission.trusted_homes().contains(&home);
        witness.trusted_home = witness
            .trust_profile
            .is_some_and(|profile| admission.is_trusted_home(&home, profile));
        PrincipalWitness::Remote(witness)
    }

    /// What `effective`, a grant of `subject`, is honoured as for a
    /// decision `subject` reaches with `evidence`.
    pub fn honour(
        &self,
        subject: &BareJid,
        evidence: &PrincipalEvidence,
        effective: RoomAffiliation,
    ) -> RoomAffiliation {
        self.witness(subject, evidence).honour(effective)
    }
}

/// A policy with S2S off, for suites that decide without a snapshot. A
/// replica builds its policy from the snapshot it runs
/// (`AppState::grant_principals`), so production code names
/// [`PrincipalPolicy::local`] where it means S2S off.
#[cfg(feature = "test-support")]
impl From<ServedAccountDomains> for PrincipalPolicy {
    fn from(served: ServedAccountDomains) -> Self {
        Self::local(served)
    }
}

/// See the conversion from an owned [`ServedAccountDomains`].
#[cfg(feature = "test-support")]
impl From<&ServedAccountDomains> for PrincipalPolicy {
    fn from(served: &ServedAccountDomains) -> Self {
        Self::local(served.clone())
    }
}

impl From<&PrincipalPolicy> for PrincipalPolicy {
    fn from(principals: &PrincipalPolicy) -> Self {
        principals.clone()
    }
}

/// What an inbound S2S stream proved about the principal whose request it
/// carried: how its domain authenticated and, when it presented a
/// certificate, the trust profile that certificate was verified under.
///
/// Outside test support it is built only from a [`RemotePrincipal`], which
/// only a verified inbound stream produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamEvidence {
    strength: AuthenticationStrength,
    trust_profile: Option<TrustProfileKind>,
}

impl StreamEvidence {
    /// The evidence of the stream `principal` arrived on.
    pub fn of(principal: &RemotePrincipal) -> Self {
        Self {
            strength: principal.strength(),
            trust_profile: principal.trust_profile(),
        }
    }

    /// The evidence a seat recorded when its join was admitted: what the
    /// seat's row holds, written from the [`RemotePrincipal`] whose join
    /// established it. Only the seat store reads rows into evidence.
    pub(crate) fn recorded(
        strength: AuthenticationStrength,
        trust_profile: Option<TrustProfileKind>,
    ) -> Self {
        Self {
            strength,
            trust_profile,
        }
    }

    /// Evidence as given, for suites that decide without a live stream.
    #[cfg(feature = "test-support")]
    pub fn for_test(
        strength: AuthenticationStrength,
        trust_profile: Option<TrustProfileKind>,
    ) -> Self {
        Self {
            strength,
            trust_profile,
        }
    }

    pub fn strength(&self) -> AuthenticationStrength {
        self.strength
    }

    pub fn trust_profile(&self) -> Option<TrustProfileKind> {
        self.trust_profile
    }
}

/// The stream evidence the seats of remote occupants of local rooms were
/// admitted under, as the replica that seats them holds it.
///
/// A remote occupant's seat is established by a join an inbound S2S stream
/// carried, and records what that stream proved. A decision that names a
/// seated remote occupant without a request of its own (the audience of a
/// room acceptance, the revocation sweep, the seats restored after a room's
/// claim moved) decides it on that evidence instead of on none. Admission
/// and trusted-home status are still evaluated at each decision, from the
/// snapshot the replica runs: a seat records only what its stream proved.
pub trait SeatEvidence: Send + Sync {
    /// The evidence of every seat `account` holds in `room` on this
    /// replica; empty when it holds none here.
    fn of_account(&self, room: &BareJid, account: &BareJid) -> Vec<StreamEvidence>;
}

impl std::fmt::Debug for dyn SeatEvidence + '_ {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SeatEvidence")
    }
}

/// A replica that seats no remote occupant: S2S is off, or the decision is
/// made where no seat is held.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSeats;

impl SeatEvidence for NoSeats {
    fn of_account(&self, _room: &BareJid, _account: &BareJid) -> Vec<StreamEvidence> {
        Vec::new()
    }
}

/// The evidence a decision that names `account` in `room` without its
/// request decides it on: that of each seat it holds there, or no stream
/// when it holds none. The account is admitted only when every one of them
/// admits it, so an occupant seated on streams of different strength is
/// decided by the weakest.
pub fn named_evidence(
    seats: &dyn SeatEvidence,
    room: &BareJid,
    account: &BareJid,
) -> Vec<PrincipalEvidence> {
    let seated = seats.of_account(room, account);
    if seated.is_empty() {
        return vec![PrincipalEvidence::NoStream];
    }
    seated.into_iter().map(PrincipalEvidence::Stream).collect()
}

/// How the principal of a decision reached it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalEvidence {
    /// No S2S stream carried a request of the principal to this decision:
    /// a local session's own request, or a principal the decision names
    /// without its request (a recipient, a seated occupant, a listed
    /// subject).
    NoStream,
    /// An inbound S2S stream carried the principal's request.
    Stream(StreamEvidence),
}

impl PrincipalEvidence {
    /// The evidence of the stream `principal` arrived on.
    pub fn of(principal: &RemotePrincipal) -> Self {
        Self::Stream(StreamEvidence::of(principal))
    }
}

/// Why a remote principal's domain is not admitted at a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteRefusal {
    /// S2S is off: the replica runs no snapshot and admits no remote
    /// domain.
    S2sDisabled,
    /// The principal's domain is not in the identity profile's canonical
    /// form.
    NotCanonical,
    /// A stream claimed an account of a domain this deployment serves.
    ServedDomainOnStream,
    /// The snapshot's admission policy refuses the domain (ADR-0020 D1),
    /// a denylisted parent domain included.
    Admission(AdmissionRefusal),
}

/// The admission witness of a remote principal (ADR-0020 gate G17), as
/// evaluated at one decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteWitness {
    home: String,
    strength: Option<AuthenticationStrength>,
    trust_profile: Option<TrustProfileKind>,
    snapshot: Option<Uuid>,
    admission: Result<(), RemoteRefusal>,
    trusted_home_listed: bool,
    trusted_home: bool,
    pkix_required: bool,
}

impl RemoteWitness {
    /// The principal's home domain, as the subject names it.
    pub fn home(&self) -> &str {
        &self.home
    }

    /// The authentication strength of the stream that carried the
    /// principal's request; `None` when no stream did.
    pub fn strength(&self) -> Option<AuthenticationStrength> {
        self.strength
    }

    /// The trust profile that stream's certificate was verified under;
    /// `None` without a stream or without a certificate.
    pub fn trust_profile(&self) -> Option<TrustProfileKind> {
        self.trust_profile
    }

    /// The S2S snapshot the decision read; `None` while S2S is off.
    pub fn snapshot(&self) -> Option<Uuid> {
        self.snapshot
    }

    /// The D1 admission decision for the home domain.
    pub fn admission(&self) -> Result<(), RemoteRefusal> {
        self.admission
    }

    /// Whether the home is on the snapshot's trusted-home list.
    pub fn trusted_home_listed(&self) -> bool {
        self.trusted_home_listed
    }

    /// Whether the home is a trusted home for the stream that carried the
    /// request: admitted, listed and, where the operator requires a
    /// private-CA profile for it, verified under one. `false` without a
    /// stream certificate.
    pub fn trusted_home(&self) -> bool {
        self.trusted_home
    }

    /// Whether host policy requires `pkix` strength for any grant.
    pub fn pkix_required(&self) -> bool {
        self.pkix_required
    }

    fn pkix(&self) -> bool {
        self.strength == Some(AuthenticationStrength::Pkix) && self.trust_profile.is_some()
    }

    /// Whether any grant of the principal is honoured: its domain is
    /// admitted and the stream meets host policy's strength requirement.
    pub fn grants_honoured(&self) -> bool {
        self.admission.is_ok() && (!self.pkix_required || self.pkix())
    }

    /// Whether a privileged or moderator grant of the principal is
    /// honoured (ADR-0020 D8): a trusted home on a `pkix` stream.
    pub fn privilege_honoured(&self) -> bool {
        self.grants_honoured() && self.trusted_home && self.pkix()
    }
}

/// What the principal of a decision is, as the decision honours its
/// grants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrincipalWitness {
    /// An account of a domain this deployment serves, deciding without an
    /// S2S stream.
    Served,
    /// Any other principal.
    Remote(RemoteWitness),
}

impl PrincipalWitness {
    /// What an effective affiliation is honoured as for this principal. A
    /// grant that is not honoured is honoured as `None`, which allows
    /// nothing; a ban is always honoured.
    pub fn honour(&self, effective: RoomAffiliation) -> RoomAffiliation {
        let Self::Remote(witness) = self else {
            return effective;
        };
        if effective == RoomAffiliation::Outcast {
            return RoomAffiliation::Outcast;
        }
        if !witness.grants_honoured() {
            return RoomAffiliation::None;
        }
        if effective.confers_privilege_or_moderation() && !witness.privilege_honoured() {
            return RoomAffiliation::None;
        }
        effective
    }

    /// Whether a privileged or moderator grant, or the moderator role, of
    /// this principal is honoured: always for a served account, and for a
    /// remote principal only as [`RemoteWitness::privilege_honoured`] says.
    pub fn privilege_honoured(&self) -> bool {
        match self {
            Self::Served => true,
            Self::Remote(witness) => witness.privilege_honoured(),
        }
    }

    /// Whether any grant, or any standing a room's own checks give, is
    /// honoured for this principal: always for a served account, and for a
    /// remote principal only while its domain is admitted and its stream
    /// meets host policy's strength requirement
    /// ([`RemoteWitness::grants_honoured`]).
    pub fn grants_honoured(&self) -> bool {
        match self {
            Self::Served => true,
            Self::Remote(witness) => witness.grants_honoured(),
        }
    }

    /// The remote witness, for a remote principal.
    pub fn remote(&self) -> Option<&RemoteWitness> {
        match self {
            Self::Served => None,
            Self::Remote(witness) => Some(witness),
        }
    }
}
