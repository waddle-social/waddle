//! Shared fixture of the host-grant suites: one isolated PostgreSQL database
//! with the migration catalog applied and one enrolled scope.
//!
//! The scope manages one members-only room. `owner@example.com` owns it, and
//! the ingress fixture's sender (`romeo@example.com`) and `alice@example.com`
//! are members. Identifiers carry a per-fixture tag, so suites that share a
//! SpiceDB do not meet in it.

use jid::{BareJid, FullJid};
use uuid::Uuid;
use waddle_server::ingress::effects::room::RoomFenceRequirement;
use waddle_server::ingress::effects::ExternalEffect;
use waddle_server::ingress::{
    FrozenRoomManifest, IngressDecision, IngressSubmission, RoomExecutionPath, RoomManifestSnapshot,
};
use waddle_server::ingress_uow::CanonicalMessageRepository;
use waddle_server::permissions::host_grant::{
    Activation, BootstrapGrant, EnrollScope, GrantGeneration, GrantOperationId, GrantSubject,
    HostGrantError, HostGrantStore, IssuancePolicy, ManagedRoom, MutationReceipt, RoomAffiliation,
    ScopeKey, ScopeLifecycleId, ScopeState, SetAffiliation,
};
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, EffectMessageIdentity, EntityGeneration, IngressEffectIntent,
    MessageKey, NormalizedTarget,
};
use waddle_xmpp::muc::durable::AdminMutationId;
use waddle_xmpp::muc::room_actor::OccupancyWatermark;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::ingress_support::IngressFixture;

/// The account that owns every fixture room.
pub const OWNER: &str = "owner@example.com";
/// The ingress fixture's sender, a member of every fixture room.
pub const SENDER: &str = "romeo@example.com";
/// A member of every fixture room, seated as the sender's audience.
pub const PEER: &str = "alice@example.com";
/// The peer's seated resource.
pub const PEER_RESOURCE: &str = "alice@example.com/phone";

pub fn bare(jid: &str) -> BareJid {
    jid.parse().expect("bare JID")
}

pub fn subject(jid: &str) -> GrantSubject {
    GrantSubject::new(bare(jid)).expect("grant subject")
}

fn domain(name: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(name).expect("canonical domain")
}

/// The deployment's own domains and the given trusted homes.
pub fn issuance_policy(trusted_homes: &[&str]) -> IssuancePolicy {
    IssuancePolicy::new(
        [domain("example.com")],
        [domain("muc.example.com"), domain("spaces.example.com")],
        trusted_homes.iter().map(|home| domain(home)),
    )
    .expect("issuance policy")
}

/// One enrolled scope of a fixture.
#[derive(Clone)]
pub struct EnrolledScope {
    pub lifecycle: ScopeLifecycleId,
    pub room: BareJid,
    pub channel_id: String,
    pub space_node: String,
}

pub struct GrantFixture {
    pub ingress: IngressFixture,
    pub store: HostGrantStore,
    pub policy: IssuancePolicy,
    pub scope: EnrolledScope,
}

impl GrantFixture {
    /// A database with one enrolled scope that is still closed: its first
    /// projection has not been activated. `None` when the lane does not
    /// provide PostgreSQL and does not require it.
    pub async fn enrolled(label: &str) -> Option<Self> {
        let ingress = IngressFixture::postgres(label).await?;
        let store = HostGrantStore::open(ingress.db.clone()).expect("host-grant store");
        let policy = issuance_policy(&[]);
        let scope = enroll(&store, &policy).await;
        Some(Self {
            ingress,
            store,
            policy,
            scope,
        })
    }

    /// A database with one enrolled scope whose bootstrap grants are
    /// effective. The fixture activates the scope itself, standing in for a
    /// verified projection: these suites exercise the canonical store and
    /// SQL admission, which do not read SpiceDB.
    pub async fn active(label: &str) -> Option<Self> {
        let fixture = Self::enrolled(label).await?;
        assert_eq!(
            fixture.activate_current(&fixture.scope).await,
            Activation::Activated
        );
        Some(fixture)
    }

    /// Enroll another scope, with its own room, in the same database.
    pub async fn enroll_another(&self) -> EnrolledScope {
        enroll(&self.store, &self.policy).await
    }

    pub async fn state(&self, scope: &EnrolledScope) -> ScopeState {
        self.store
            .scope(scope.lifecycle)
            .await
            .expect("read scope")
            .expect("enrolled scope")
    }

    pub async fn generation(&self) -> GrantGeneration {
        self.state(&self.scope).await.generation
    }

    /// A change to the fixture's first scope, decided at its current
    /// generation under a fresh operation identity.
    pub async fn command(
        &self,
        initiator: &str,
        target: &str,
        affiliation: RoomAffiliation,
    ) -> SetAffiliation {
        SetAffiliation {
            operation: GrantOperationId::generate(),
            lifecycle: self.scope.lifecycle,
            expected_generation: self.generation().await,
            initiator: subject(initiator),
            room: self.scope.room.clone(),
            subject: subject(target),
            affiliation,
        }
    }

    /// Record a change to the fixture's first scope.
    pub async fn set(
        &self,
        initiator: &str,
        target: &str,
        affiliation: RoomAffiliation,
    ) -> Result<MutationReceipt, HostGrantError> {
        let command = self.command(initiator, target, affiliation).await;
        self.store.set_affiliation(&command, &self.policy).await
    }

    /// Activate a scope's current operation without a projection.
    pub async fn activate_current(&self, scope: &EnrolledScope) -> Activation {
        let plan = self
            .store
            .projection_plan(scope.lifecycle)
            .await
            .expect("projection plan");
        self.store
            .activate(plan.marker())
            .await
            .expect("activation")
    }

    /// The recorded and the effective affiliation of an account in the
    /// fixture's first room; `None` when it holds no row.
    pub async fn affiliations(&self, account: &str) -> Option<(RoomAffiliation, RoomAffiliation)> {
        self.store
            .grants(self.scope.lifecycle)
            .await
            .expect("grant rows")
            .into_iter()
            .find(|grant| grant.room == self.scope.room && grant.subject.as_str() == account)
            .map(|grant| (grant.affiliation, grant.effective_affiliation))
    }

    /// A groupchat from the fixture's sender to the first scope's room, with
    /// the peer seated beside it.
    pub fn room_submission(&self, origin: &str, body: &str) -> IngressSubmission {
        room_submission(
            &self.ingress,
            &self.scope.room,
            origin,
            body,
            &[PEER_RESOURCE],
        )
    }

    /// The manifest sealed for a canonical message, if any.
    pub async fn manifest(&self, key: MessageKey) -> Option<FrozenRoomManifest> {
        sealed_manifest(&self.ingress, key).await
    }

    /// The row counts of every relation an acceptance writes.
    pub async fn accepted_rows(&self) -> [i64; 4] {
        [
            self.ingress.count("ingress_messages").await,
            self.ingress.count("ingress_effect_intents").await,
            self.ingress.count("ingress_effect_receipts").await,
            self.ingress.count("mam_messages").await,
        ]
    }

    pub async fn close(self) {
        drop(self.store);
        self.ingress.close().await;
    }
}

/// Enroll a fresh scope with one room, owned by [`OWNER`], with the sender
/// and the peer as members.
async fn enroll(store: &HostGrantStore, policy: &IssuancePolicy) -> EnrolledScope {
    let tag = Uuid::new_v4().simple().to_string();
    let room = bare(&format!("room-{tag}@muc.example.com"));
    let managed = ManagedRoom::new(room.clone()).expect("managed room");
    let scope = EnrolledScope {
        lifecycle: ScopeLifecycleId::generate(),
        channel_id: managed.channel_id().to_owned(),
        room: room.clone(),
        space_node: format!("space-{tag}"),
    };
    let grant = |account: &str, affiliation| BootstrapGrant {
        room: room.clone(),
        subject: subject(account),
        affiliation,
    };
    let receipt = store
        .enroll_scope(
            &EnrollScope {
                operation: GrantOperationId::generate(),
                lifecycle: scope.lifecycle,
                key: ScopeKey::new(bare("spaces.example.com"), scope.space_node.as_str())
                    .expect("scope key"),
                rooms: vec![managed],
                bootstrap: vec![
                    grant(OWNER, RoomAffiliation::Owner),
                    grant(SENDER, RoomAffiliation::Member),
                    grant(PEER, RoomAffiliation::Member),
                ],
            },
            policy,
        )
        .await
        .expect("enroll scope");
    assert!(matches!(receipt, MutationReceipt::Recorded(_)));
    scope
}

/// A groupchat from the ingress fixture's sender to `room`, planned as the
/// room planner plans it: one route to the seated occupants, the sender's
/// reflection, and the manifest snapshot of that occupancy.
pub fn room_submission(
    ingress: &IngressFixture,
    room: &BareJid,
    origin: &str,
    body: &str,
    peers: &[&str],
) -> IngressSubmission {
    let mut submission = ingress.submission(Some(origin), body);
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.type_ = MessageType::Groupchat;
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let stamp = StanzaId::new(format!("stamp-{origin}"), room.clone().into());
    let mut canonical = submission.plan.sanitized_message.clone();
    canonical.from = Some(room.with_resource_str("romeo").expect("nick").into());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut canonical, &stamp);
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
        &mut canonical,
        &waddle_xmpp::xep::xep0421::OccupantId("sender-occupancy".into()),
    );
    submission.plan.room_canonical_message = Some(Box::new(canonical));
    let mut occupants = vec![submission.sender.clone()];
    occupants.extend(
        peers
            .iter()
            .map(|peer| peer.parse::<FullJid>().expect("occupant")),
    );
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: occupants.clone(),
            reflection: submission.sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::stanza(stamp),
        });
    submission.plan.room_execution = RoomExecutionPath::Local {
        room: room.clone(),
        client_origin: None,
        fence: RoomFenceRequirement::Unfenced,
        snapshot_generation: 1,
        manifest_snapshot: Some(Box::new(RoomManifestSnapshot {
            seats: Vec::new(),
            host_no_transport: Vec::new(),
            coordinates: None,
            occupancy_incarnation: AdminMutationId::generate(),
            occupants: occupants
                .into_iter()
                .zip(1_u64..)
                .map(|(occupant, revision)| (occupant, OccupancyWatermark::from_revision(revision)))
                .collect(),
        })),
    };
    submission
}

/// The manifest sealed for a canonical message, if any.
pub async fn sealed_manifest(
    ingress: &IngressFixture,
    key: MessageKey,
) -> Option<FrozenRoomManifest> {
    let mut tx = ingress.uow.begin().await.expect("read transaction");
    let manifest = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope query")
        .and_then(|envelope| envelope.room_manifest().cloned());
    tx.commit().await.expect("read commit");
    manifest
}

/// The stanza error a committed refusal replies with: its only external
/// effect is the error frame to the sender.
pub fn refusal(decision: &IngressDecision) -> (ErrorType, DefinedCondition) {
    let [ExternalEffect::Frame(frame)] = decision.external.as_slice() else {
        panic!(
            "a refusal sends exactly one frame, got {} effects",
            decision.external.len()
        );
    };
    let Stanza::Message(message) = frame.as_ref() else {
        panic!("a refusal replies with a message stanza");
    };
    assert_eq!(message.type_, MessageType::Error);
    let error = message
        .payloads
        .iter()
        .find_map(|payload| StanzaError::try_from(payload.clone()).ok())
        .expect("stanza error payload");
    (error.type_, error.defined_condition)
}
