//! Typed identities, commands and records of the host-grant authority.

use jid::{BareJid, Jid};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::identity::CanonicalBareJid;

use super::error::HostGrantError;
use super::issuance::IssuanceBasis;
use crate::permissions::{AuthzUserJid, MAX_AUTHZ_ID_BYTES};

/// The version of the admission policy this build evaluates. A scope
/// enrolled under another version is not admitted.
pub const HOST_GRANT_POLICY_VERSION: u32 = 1;

/// Most rooms one scope manages. With [`HOST_GRANT_SCOPE_MAX_GRANT_ROWS`] it
/// keeps every projection write and its preconditions inside one bounded
/// `WriteRelationships` request.
pub const HOST_GRANT_SCOPE_MAX_ROOMS: usize = 8;

/// Most grant rows one scope holds, counting revoked subjects: a revoke is a
/// row, never a delete. One projection write carries four relationship
/// updates per row and the marker, 802 updates at the bound.
pub const HOST_GRANT_SCOPE_MAX_GRANT_ROWS: usize = 200;

macro_rules! uuid_identity {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            /// A fresh random identity.
            pub fn generate() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }

            /// The stored and projected spelling: 32 lowercase hex digits.
            pub fn to_storage(self) -> String {
                self.0.simple().to_string()
            }

            /// Parse exactly the stored spelling.
            pub fn from_storage(value: &str) -> Option<Self> {
                let canonical = value.len() == 32
                    && value
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
                if !canonical {
                    return None;
                }
                Uuid::try_parse(value).ok().map(Self)
            }
        }
    };
}

uuid_identity!(
    /// One enrollment of a scope key. It is recorded in an append-only ledger
    /// and never reused: a scope key holds a single lifecycle.
    ScopeLifecycleId
);

uuid_identity!(
    /// The authority incarnation a scope and its projection marker are bound
    /// to. A scope whose incarnation is not the deployment's current one is
    /// not admitted.
    AuthorityIncarnation
);

uuid_identity!(
    /// The stable internal identity of one operation. An exact retry under
    /// the same identity returns the recorded outcome.
    GrantOperationId
);

/// A scope's admission generation. Every recorded operation advances it by
/// one; it never decreases and no value is used twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GrantGeneration(i64);

impl GrantGeneration {
    /// The generation an enrollment records.
    pub const FIRST: Self = Self(1);

    /// The generation a scope is closed at for good once no working
    /// generation remains.
    pub const TERMINAL: Self = Self(i64::MAX);

    pub fn new(value: i64) -> Option<Self> {
        (value > 0).then_some(Self(value))
    }

    pub fn value(self) -> i64 {
        self.0
    }

    /// The generation the next operation records, or `None` when only
    /// [`Self::TERMINAL`] remains.
    pub fn next(self) -> Option<Self> {
        if self.0 >= i64::MAX - 1 {
            None
        } else {
            Some(Self(self.0 + 1))
        }
    }
}

/// SHA-256 of a scope's intended projection at one generation, as 64
/// lowercase hex digits.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectionDigest(String);

impl ProjectionDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_storage(value: &str) -> Option<Self> {
        let canonical = value.len() == 64
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        canonical.then(|| Self(value.to_owned()))
    }
}

/// A room affiliation, ordered by the access it grants: `Outcast` denies
/// everything, `None` grants nothing, and each later variant grants more.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RoomAffiliation {
    Outcast,
    None,
    Member,
    Admin,
    Owner,
}

impl RoomAffiliation {
    pub const ALL: [Self; 5] = [
        Self::Outcast,
        Self::None,
        Self::Member,
        Self::Admin,
        Self::Owner,
    ];

    /// The affiliations that hold a relationship in the projection.
    pub const PROJECTED: [Self; 4] = [Self::Owner, Self::Admin, Self::Member, Self::Outcast];

    pub fn as_storage(self) -> &'static str {
        match self {
            Self::Outcast => "outcast",
            Self::None => "none",
            Self::Member => "member",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|affiliation| affiliation.as_storage() == value)
    }

    /// The channel relation that projects this affiliation; no affiliation
    /// projects no relationship.
    pub fn relation(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            other => Some(other.as_storage()),
        }
    }

    /// Whether the affiliation is a privileged grant or confers room
    /// moderation (ADR-0020 D8). Owner and admin confer `manage` and
    /// `moderate`; member, none and outcast do not.
    pub fn confers_privilege_or_moderation(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

/// Why a JID cannot be an account-grant subject.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GrantSubjectError {
    #[error("a grant subject is a bare account JID, not a full JID")]
    FullJid,
    #[error("a grant subject is an account, not a service identity")]
    ServiceIdentity,
    #[error("a grant subject must be a canonical bare account JID within the codec limits")]
    NotCanonical,
}

/// The subject of a grant: a bare account JID in the identity profile's
/// canonical form, which is also its SQL grant key and its projected subject.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GrantSubject(BareJid);

impl GrantSubject {
    /// Accept a bare account JID in canonical form. A JID without a
    /// localpart names a service, not an account.
    pub fn new(jid: BareJid) -> Result<Self, GrantSubjectError> {
        if jid.node().is_none() {
            return Err(GrantSubjectError::ServiceIdentity);
        }
        let account = AuthzUserJid::new(jid).map_err(|_| GrantSubjectError::NotCanonical)?;
        Ok(Self(account.jid().clone()))
    }

    /// Classify any JID offered as a grant subject. A full JID is refused
    /// rather than reduced to its bare JID.
    pub fn from_jid(jid: &Jid) -> Result<Self, GrantSubjectError> {
        match jid.clone().try_into_full() {
            Ok(_) => Err(GrantSubjectError::FullJid),
            Err(bare) => Self::new(bare),
        }
    }

    pub fn jid(&self) -> &BareJid {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn from_storage(value: &str) -> Option<Self> {
        AuthzUserJid::parse_exact(value)
            .ok()
            .map(|account| Self(account.jid().clone()))
    }
}

/// A community: the canonical service JID and the opaque Space node.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScopeKey {
    service: BareJid,
    node: String,
}

impl ScopeKey {
    /// `service` must be in the identity profile's canonical form and `node`
    /// a nonempty identifier within the authorization codec's limit.
    pub fn new(service: BareJid, node: impl Into<String>) -> Result<Self, HostGrantError> {
        let node = node.into();
        if node.is_empty() || node.len() > MAX_AUTHZ_ID_BYTES {
            return Err(HostGrantError::InvalidScopeKey);
        }
        let service = CanonicalBareJid::new(service)
            .map_err(|_| HostGrantError::InvalidScopeKey)?
            .into_bare_jid();
        Ok(Self { service, node })
    }

    pub fn service(&self) -> &BareJid {
        &self.service
    }

    pub fn node(&self) -> &str {
        &self.node
    }
}

/// A managed room and the authorization object its affiliations project to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ManagedRoom {
    room: BareJid,
    channel_id: String,
}

impl ManagedRoom {
    /// `room` must be a canonical managed room JID.
    pub fn new(room: BareJid) -> Result<Self, HostGrantError> {
        let room = CanonicalBareJid::new(room)
            .map_err(|_| HostGrantError::UnmanagedRoom)?
            .into_bare_jid();
        let channel_id =
            waddle_xmpp::parse_managed_room_jid(&room).ok_or(HostGrantError::UnmanagedRoom)?;
        if channel_id.len() > MAX_AUTHZ_ID_BYTES {
            return Err(HostGrantError::UnmanagedRoom);
        }
        Ok(Self { room, channel_id })
    }

    pub(crate) fn from_storage(room: &str, channel_id: &str) -> Option<Self> {
        let parsed: BareJid = room.parse().ok()?;
        (parsed.as_str() == room && !channel_id.is_empty()).then(|| Self {
            room: parsed,
            channel_id: channel_id.to_owned(),
        })
    }

    pub fn room(&self) -> &BareJid {
        &self.room
    }

    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }
}

/// A grant recorded with a scope's enrollment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapGrant {
    pub room: BareJid,
    pub subject: GrantSubject,
    pub affiliation: RoomAffiliation,
}

/// Enroll a fresh scope with its rooms and bootstrap grants. It is an
/// operator act: the transaction writes relations the runtime role cannot
/// insert into.
#[derive(Clone, Debug)]
pub struct EnrollScope {
    pub operation: GrantOperationId,
    pub lifecycle: ScopeLifecycleId,
    pub key: ScopeKey,
    pub rooms: Vec<ManagedRoom>,
    pub bootstrap: Vec<BootstrapGrant>,
}

/// Set one subject's affiliation in one managed room.
#[derive(Clone, Debug)]
pub struct SetAffiliation {
    pub operation: GrantOperationId,
    pub lifecycle: ScopeLifecycleId,
    /// The scope generation the initiator decided under.
    pub expected_generation: GrantGeneration,
    /// The account that asks for the change. Its authority is revalidated
    /// under the scope's guard, in the transaction that records the change.
    pub initiator: GrantSubject,
    pub room: BareJid,
    pub subject: GrantSubject,
    pub affiliation: RoomAffiliation,
}

/// Close a scope for good. Its key is not enrolled again.
#[derive(Clone, Debug)]
pub struct RetireScope {
    pub operation: GrantOperationId,
    pub lifecycle: ScopeLifecycleId,
    pub expected_generation: GrantGeneration,
}

/// What an operation recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Enroll,
    SetAffiliation,
    Retire,
    /// The scope had no working generation left for the command and was
    /// closed instead.
    Exhaust,
}

impl OperationKind {
    pub(crate) fn as_storage(self) -> &'static str {
        match self {
            Self::Enroll => "enroll",
            Self::SetAffiliation => "set_affiliation",
            Self::Retire => "retire",
            Self::Exhaust => "exhaust",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Option<Self> {
        [
            Self::Enroll,
            Self::SetAffiliation,
            Self::Retire,
            Self::Exhaust,
        ]
        .into_iter()
        .find(|kind| kind.as_storage() == value)
    }
}

/// The immutable outcome of a recorded operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationRecord {
    pub operation: GrantOperationId,
    pub lifecycle: ScopeLifecycleId,
    pub generation: GrantGeneration,
    pub kind: OperationKind,
    pub authority_incarnation: AuthorityIncarnation,
    pub projection_digest: ProjectionDigest,
}

/// The result of a mutation whose commit was acknowledged, or of its exact
/// retry. Neither says the change is effective: an increase of access waits
/// for its projection to be verified and activated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutationReceipt {
    /// This call recorded the operation.
    Recorded(OperationRecord),
    /// The operation was already recorded with the same input; this is its
    /// original outcome and nothing was authorized again.
    Replayed(OperationRecord),
}

impl MutationReceipt {
    pub fn record(&self) -> &OperationRecord {
        match self {
            Self::Recorded(record) | Self::Replayed(record) => record,
        }
    }
}

/// What the canonical store holds for an operation identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationOutcome {
    Committed(OperationRecord),
    /// No operation is recorded under the identity: it did not commit.
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionState {
    Closed,
    Active,
}

/// Why a scope admits nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClosedReason {
    /// Enrolled; its first projection has not been verified and activated.
    Enrolled,
    Retired,
    Exhausted,
}

impl ClosedReason {
    pub(crate) fn as_storage(self) -> &'static str {
        match self {
            Self::Enrolled => "enrolled",
            Self::Retired => "retired",
            Self::Exhausted => "exhausted",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Option<Self> {
        [Self::Enrolled, Self::Retired, Self::Exhausted]
            .into_iter()
            .find(|reason| reason.as_storage() == value)
    }
}

/// The latest state of a scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeState {
    pub lifecycle: ScopeLifecycleId,
    pub authority_incarnation: AuthorityIncarnation,
    pub policy_version: u32,
    pub generation: GrantGeneration,
    /// The operation that recorded `generation`.
    pub operation: GrantOperationId,
    /// The latest generation whose projection was verified and activated.
    pub projected_generation: Option<GrantGeneration>,
    pub admission: AdmissionState,
    pub closed_reason: Option<ClosedReason>,
}

impl ScopeState {
    /// Whether an operation is recorded whose projection is not yet
    /// verified and activated.
    pub fn projection_pending(&self) -> bool {
        self.projected_generation != Some(self.generation)
    }
}

/// The latest grant of one subject in one room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRow {
    pub room: BareJid,
    pub subject: GrantSubject,
    /// The recorded intent.
    pub affiliation: RoomAffiliation,
    /// What admission honours: never more than `affiliation`, and less
    /// while an increase waits for its projection.
    pub effective_affiliation: RoomAffiliation,
    pub changed_generation: GrantGeneration,
    pub operation: GrantOperationId,
    pub issuance_basis: IssuanceBasis,
}

/// A read of the policy state outside any acceptance transaction. It is a
/// candidate only: acceptance reads and locks the state again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateAdmission {
    pub admitted: bool,
    /// The scope the room is bound to, if any.
    pub lifecycle: Option<ScopeLifecycleId>,
    /// The scope generation the read saw.
    pub generation: Option<GrantGeneration>,
}

/// Length-prefixed hashing, so distinct field sequences never collide.
pub(crate) struct Fingerprint(Sha256);

impl Fingerprint {
    pub(crate) fn new(domain: &str) -> Self {
        let mut fingerprint = Self(Sha256::new());
        fingerprint.text(domain);
        fingerprint
    }

    pub(crate) fn text(&mut self, value: &str) {
        self.0.update((value.len() as u64).to_be_bytes());
        self.0.update(value.as_bytes());
    }

    pub(crate) fn number(&mut self, value: i64) {
        self.0.update(value.to_be_bytes());
    }

    pub(crate) fn finish(self) -> String {
        hex::encode(self.0.finalize())
    }
}

impl EnrollScope {
    /// The immutable fingerprint of this command's input.
    pub(crate) fn fingerprint(&self) -> String {
        let mut fingerprint = Fingerprint::new("waddle:host-grant:enroll:v1");
        fingerprint.text(&self.lifecycle.to_storage());
        fingerprint.text(self.key.service().as_str());
        fingerprint.text(self.key.node());
        let mut rooms: Vec<&str> = self.rooms.iter().map(|room| room.room().as_str()).collect();
        rooms.sort_unstable();
        fingerprint.number(rooms.len() as i64);
        for room in rooms {
            fingerprint.text(room);
        }
        let mut grants: Vec<(&str, &str, &str)> = self
            .bootstrap
            .iter()
            .map(|grant| {
                (
                    grant.room.as_str(),
                    grant.subject.as_str(),
                    grant.affiliation.as_storage(),
                )
            })
            .collect();
        grants.sort_unstable();
        fingerprint.number(grants.len() as i64);
        for (room, subject, affiliation) in grants {
            fingerprint.text(room);
            fingerprint.text(subject);
            fingerprint.text(affiliation);
        }
        fingerprint.finish()
    }
}

impl SetAffiliation {
    /// The immutable fingerprint of this command's input, including the
    /// generation it was decided under and its initiator.
    pub(crate) fn fingerprint(&self) -> String {
        let mut fingerprint = Fingerprint::new("waddle:host-grant:set-affiliation:v1");
        fingerprint.text(&self.lifecycle.to_storage());
        fingerprint.number(self.expected_generation.value());
        fingerprint.text(self.initiator.as_str());
        fingerprint.text(self.room.as_str());
        fingerprint.text(self.subject.as_str());
        fingerprint.text(self.affiliation.as_storage());
        fingerprint.finish()
    }
}

impl RetireScope {
    pub(crate) fn fingerprint(&self) -> String {
        let mut fingerprint = Fingerprint::new("waddle:host-grant:retire:v1");
        fingerprint.text(&self.lifecycle.to_storage());
        fingerprint.number(self.expected_generation.value());
        fingerprint.finish()
    }
}
