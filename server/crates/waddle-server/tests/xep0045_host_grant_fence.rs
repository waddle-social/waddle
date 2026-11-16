//! The writer fence and the sink admission of an enrolled host-grant scope
//! (ADR-0020 D5, D8; gates G11, G12, G14, G16), outside room-manifest
//! acceptance.
//!
//! A room an enrolled scope manages is decided from its canonical grants
//! on every surface: joins, archive reads, room discovery, administration,
//! owner configuration and destruction, mediated invitations, PubSub on the
//! scope's Space node, the planner's durable membership and the legacy
//! permission reads, and the protected effects of `sinks`. Legacy tuples,
//! durable room affiliations, PubSub affiliations, Space membership and the
//! room actor's own affiliation list and roles admit nothing there.
//! Bootstrap and repair writers never lift a ban and never write a
//! privileged grant for an account outside the served domain and the
//! trusted homes they are given.
//!
//! The WebSocket cases drive the production router over a loopback socket
//! against a state on the fixture's PostgreSQL database, wired as a
//! PostgreSQL deployment is: its permission actor is fenced by the store and
//! shares the enrolled set with the host-grant gate. The relationships a
//! stale mirror or an older binary would have left are written with a
//! separate unfenced actor. The other cases call the fenced writers and
//! readers directly. They need PostgreSQL only; a lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them without it. The fixture
//! activates the scope in place of a verified projection, as the other SQL
//! admission suites do, and attaches the replica without the
//! runtime-privilege check, since its login owns the schema.
#![cfg(feature = "test-support")]

// The shared host-grant fixture serves several suites; this one uses a subset.
#[allow(dead_code)]
mod host_grant_support;
// The shared ingress fixture serves several suites; this one uses a subset.
#[allow(dead_code)]
mod ingress_support;
mod ws_loopback_support;

#[path = "host_grant_fence_cases/sinks.rs"]
mod sinks;

use std::sync::Arc;
use std::time::Duration;

use host_grant_support::{bare, issuance_policy, served, GrantFixture, OWNER, PEER, SENDER};
use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use waddle_server::db::{DatabaseConfig, DatabasePool, PoolConfig};
use waddle_server::ingress::IngressAuthority;
use waddle_server::permissions::host_grant::{
    GateState, HostGrantAttachment, HostGrantGate, RestoreEpoch, RoomAffiliation, RoomBinding,
    RoomGate,
};
use waddle_server::permissions::{
    CheckPermission, ListRelations, ListSubjects, Object, ObjectType, Permission, PermissionActor,
    Relation, Subject, Tuple, WriteTuple,
};
use waddle_server::pubsub::DatabasePubSubStorage;
use waddle_server::spaces_pubsub_seed::{
    seed_owner_on_all_nodes, seed_owners_on_node, OwnerSeedOutcome, OwnerSeedPolicy,
};
use waddle_server::test_support::{
    websocket_state_with_host_grant_fence, PermissionDurableMembershipSource,
};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::stanza::{
    get, is_error, is_result, pubsub_items, pubsub_publish, pubsub_subscribe, set,
};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::muc::affiliation::DurableMembershipSource;
use waddle_xmpp::muc::room_actor::ChangeAffiliation;
use waddle_xmpp::muc::room_registry_actor::GetRoom;
use waddle_xmpp::pubsub::{Affiliation, InMemoryPubSubStorage, PubSubStorage};
use ws_loopback_support::Loopback;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::DefinedCondition;

/// An account of the served domain that holds no grant in the room.
const CAROL: &str = "carol@example.com";
/// An account of a home that is not a trusted home.
const REMOTE: &str = "eve@remote.example";
/// The test state's MUC service.
const MUC: &str = "muc.example.com";
/// The test state's Spaces service.
const SPACES: &str = "spaces.example.com";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_MAM: &str = "urn:xmpp:mam:2";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const MUC_MEMBERSONLY: &str = "muc_membersonly";

/// The fixture scope's room served over a loopback socket.
struct ManagedRoom {
    grants: GrantFixture,
    server: Loopback,
    /// The replica's ingress authority, whose attachment gate the state's
    /// host-grant gate is bound to.
    authority: Arc<IngressAuthority>,
    /// A permission actor without the host-grant fence, standing in for a
    /// stale mirror or an older binary: it writes and reads the legacy
    /// relationships the fenced actor refuses.
    legacy: ActorRef<PermissionActor>,
}

impl ManagedRoom {
    /// The fixture's room with the replica attached to the store.
    async fn start(label: &str) -> Option<Self> {
        Self::serve(label, true).await
    }

    /// The fixture's room on a replica that has not verified its runtime
    /// role's privileges on the store.
    async fn start_unattached(label: &str) -> Option<Self> {
        Self::serve(label, false).await
    }

    async fn serve(label: &str, attach: bool) -> Option<Self> {
        let grants = GrantFixture::active(label).await?;
        let pool = Arc::new(
            DatabasePool::try_new(
                DatabaseConfig::new(grants.ingress.db.driver(), grants.ingress.db.database_url()),
                PoolConfig,
            )
            .await
            .expect("shared database pool"),
        );
        let authority = Arc::new(grants.ingress.authority().await);
        let state = websocket_state_with_host_grant_fence(pool, Arc::clone(&authority)).await;
        if attach {
            // The replica's attachment gate attaches, as its verification
            // would; the surfaces read it through the state's gate.
            let attachment = HostGrantAttachment::unfenced_for_test(&grants.store, served())
                .await
                .expect("attachment");
            assert!(authority.attach_host_grants(attachment));
            assert!(state.deps.app_state.host_grants.attachment().is_some());
        }
        let legacy = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
            grants.ingress.db.clone(),
        )));
        let server = Loopback::serve(state).await;
        for account in ["owner", "romeo", "alice", "carol"] {
            server.account(account).await;
        }
        // The catalog row says the room is open; the room's binding makes it
        // members-only whatever the row says.
        server.channel(&grants.scope.channel_id, false).await;
        Some(Self {
            grants,
            server,
            authority,
            legacy,
        })
    }

    fn room(&self) -> &BareJid {
        &self.grants.scope.room
    }

    fn channel(&self) -> Object {
        Object::new(ObjectType::Channel, self.grants.scope.channel_id.as_str())
    }

    /// Reload the replica's enrolled set after the fixture enrolled
    /// another scope.
    async fn refresh_enrolled(&self) {
        self.server
            .state
            .deps
            .app_state
            .host_grants
            .enrolled()
            .refresh()
            .await
            .expect("enrolled set refresh");
    }

    /// Hand `account` every legacy grant the room's legacy stores can hold:
    /// a channel owner and member relationship and a parent Space
    /// membership. None of them admits anything in a managed room.
    async fn legacy_grants(&self, account: &str) {
        for relation in ["owner", "member"] {
            self.legacy
                .ask(WriteTuple {
                    tuple: Tuple::new(
                        self.channel(),
                        Relation::new(relation),
                        Subject::user(account),
                    ),
                })
                .await
                .expect("legacy channel relationship");
        }
        self.legacy
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Space, self.grants.scope.space_node.as_str()),
                    Relation::new("member"),
                    Subject::user(account),
                ),
            })
            .await
            .expect("legacy Space membership");
    }

    /// Give `account` an affiliation in the room actor's own list, the
    /// mirror an earlier join or administration could have left.
    async fn mirror_affiliation(&self, account: &str, affiliation: waddle_xmpp::Affiliation) {
        let room = self
            .server
            .state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: self.room().clone(),
            })
            .await
            .expect("room lookup")
            .expect("room actor");
        room.ask(ChangeAffiliation {
            jid: bare(account),
            affiliation,
        })
        .await
        .expect("mirror affiliation");
    }

    /// The legacy channel relationships the room's channel holds for
    /// `account`, read without the host-grant fence.
    async fn legacy_relations(&self, account: &str) -> Vec<String> {
        self.legacy
            .ask(ListRelations {
                subject: Subject::user(account),
                object: self.channel(),
            })
            .await
            .expect("legacy relationships")
            .into_iter()
            .map(|relation| relation.name)
            .collect()
    }

    async fn close(self) {
        drop(self.server);
        self.grants.close().await;
    }
}

fn presence_join(room: &BareJid, nick: &str) -> String {
    String::from(
        &Element::builder("presence", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                format!("{room}/{nick}"),
            )
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
}

/// Join `room` as `nick` and return the first presence the room sends
/// from that occupant address: the self-presence or the join error.
async fn join(client: &mut WsXmppClient, room: &BareJid, nick: &str) -> Element {
    client
        .send(&presence_join(room, nick))
        .await
        .expect("send join");
    let occupant = format!("{room}/{nick}");
    let frame = client
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence" && element.attr("from") == Some(occupant.as_str())
            })
        })
        .await
        .expect("join presence");
    frame.parse().expect("presence XML")
}

/// The defined condition of a presence or message error, if it is one.
fn error_condition(stanza: &Element) -> Option<String> {
    if stanza.attr("type") != Some("error") {
        return None;
    }
    stanza
        .children()
        .find(|child| child.name() == "error")
        .and_then(|error| error.children().next())
        .map(|condition| condition.name().to_owned())
}

fn admin_item(affiliation: &str, jid: &str) -> Element {
    Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    affiliation,
                )
                .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
                .build(),
        )
        .build()
}

fn disco_info() -> Element {
    Element::builder("query", NS_DISCO_INFO).build()
}

/// The reserved-nick discovery of XEP-0045 §7.12.
fn reserved_nick_query() -> Element {
    Element::builder("query", NS_DISCO_INFO)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "x-roomuser-item",
        )
        .build()
}

fn mam_query(id: &str) -> Element {
    Element::builder("query", NS_MAM)
        .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), id)
        .build()
}

fn owner_query() -> Element {
    Element::builder("query", NS_MUC_OWNER).build()
}

fn owner_destroy() -> Element {
    Element::builder("query", NS_MUC_OWNER)
        .append(Element::builder("destroy", NS_MUC_OWNER).build())
        .build()
}

fn advertises(reply: &xmpp_parsers::iq::Iq, feature: &str) -> bool {
    match reply {
        xmpp_parsers::iq::Iq::Result {
            payload: Some(query),
            ..
        } => query
            .children()
            .any(|child| child.name() == "feature" && child.attr("var") == Some(feature)),
        _ => false,
    }
}

#[tokio::test]
async fn joins_archive_reads_and_disco_follow_the_canonical_grants() {
    let Some(fixture) = ManagedRoom::start("hg_fence_sinks").await else {
        return;
    };
    let room = fixture.room().clone();
    fixture.legacy_grants(CAROL).await;

    let mut romeo = fixture.server.client("romeo", "phone").await;
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );

    // Every legacy grant the room's other stores hold admits nothing.
    let mut carol = fixture.server.client("carol", "phone").await;
    let refused = join(&mut carol, &room, "carol").await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("registration-required"),
        "an ungranted account's join: {refused:?}"
    );
    let reply = round_trip(&mut carol, set("mam", room.as_str(), mam_query("q-carol"))).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "an ungranted account read the archive: {reply:?}"
    );
    let reply = round_trip(&mut carol, get("disco", room.as_str(), disco_info())).await;
    assert!(
        is_error(&reply, DefinedCondition::ItemNotFound),
        "an ungranted account was shown the room: {reply:?}"
    );
    // The reserved-nick node (XEP-0045 §7.12) is the room's as well.
    let reply = round_trip(
        &mut carol,
        get("disco-nick", room.as_str(), reserved_nick_query()),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::ItemNotFound),
        "an ungranted account was answered the reserved-nick node: {reply:?}"
    );

    // A member reads the archive and is shown a members-only room, though
    // the catalog row says the room is open.
    let reply = round_trip(&mut romeo, set("mam", room.as_str(), mam_query("q-romeo"))).await;
    assert!(is_result(&reply), "a member's archive read: {reply:?}");
    let reply = round_trip(&mut romeo, get("disco", room.as_str(), disco_info())).await;
    assert!(
        advertises(&reply, MUC_MEMBERSONLY),
        "room disco#info: {reply:?}"
    );
    let reply = round_trip(
        &mut romeo,
        get("disco-nick", room.as_str(), reserved_nick_query()),
    )
    .await;
    assert!(
        is_result(&reply),
        "a member's reserved-nick query: {reply:?}"
    );

    // A ban recorded in the store takes effect at once, whatever the room
    // actor's own list still says.
    fixture
        .grants
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    fixture
        .mirror_affiliation(PEER, waddle_xmpp::Affiliation::Member)
        .await;
    fixture.legacy_grants(PEER).await;
    let mut alice = fixture.server.client("alice", "phone").await;
    let banned = join(&mut alice, &room, "alice").await;
    assert_eq!(
        error_condition(&banned).as_deref(),
        Some("forbidden"),
        "a banned member's join: {banned:?}"
    );
    let reply = round_trip(&mut alice, set("mam", room.as_str(), mam_query("q-alice"))).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a banned member read the archive: {reply:?}"
    );
    fixture.close().await;
}

/// Wait until the replica's attachment gate is in `state`.
async fn gate_reaches(fixture: &ManagedRoom, state: GateState, within: Duration) {
    let gate = fixture.authority.host_grant_gate();
    let deadline = tokio::time::Instant::now() + within;
    while gate.state() != state {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the attachment gate did not reach {state:?}; it is {:?}",
            gate.state()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The surfaces decide a managed room with the replica's attachment only
/// while its attachment gate is attached. When the gate verifies a changed
/// authority again and the restore fence refuses it (here the database is
/// adopted under the running replica), the gate keeps the earlier
/// attachment for acceptance to compare with, and joins, archive reads and
/// the gate's own decisions are refused until a verification passes again.
#[tokio::test]
async fn a_refused_reverification_closes_the_surfaces_until_the_fence_passes_again() {
    let Some(fixture) = ManagedRoom::start("hg_fence_reverify").await else {
        return;
    };
    let room = fixture.room().clone();
    let host_grants = fixture.server.state.deps.app_state.host_grants.clone();
    let gate = fixture.authority.host_grant_gate();
    // The incarnation binds the database's lineage, so an adoption is
    // refused by the fence. The verifier runs the fence without the
    // privilege check, since the fixture's login owns the schema.
    fixture
        .grants
        .store
        .prepare_restore_incarnation(RestoreEpoch::INITIAL)
        .await
        .expect("lineage binding");
    gate.install_verifier_for_test(
        fixture.grants.store.clone(),
        fixture.legacy.clone(),
        served(),
        RestoreEpoch::INITIAL,
    );
    assert_eq!(gate.state(), GateState::Attached);

    let mut romeo = fixture.server.client("romeo", "phone").await;
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );
    let reply = round_trip(&mut romeo, set("mam", room.as_str(), mam_query("q-before"))).await;
    assert!(is_result(&reply), "a member's archive read: {reply:?}");

    // The database is adopted under the running replica, and the gate is
    // told the authority changed, as an acceptance that read another
    // binding tells it. The fence refuses the adopted lineage.
    let pool = sqlx::PgPool::connect(fixture.grants.ingress.db.database_url())
        .await
        .expect("owner connection");
    let lineage: String = sqlx::query_scalar("SELECT lineage_uuid FROM _lineage WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("lineage");
    sqlx::query("UPDATE _lineage SET lineage_uuid = $1 WHERE id = 1")
        .bind(uuid::Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .expect("adoption");
    gate.authority_changed();
    gate_reaches(&fixture, GateState::ReverifyFailed, Duration::from_secs(10)).await;
    // The earlier attachment is still held, for acceptance to compare with;
    // the surfaces do not read it.
    assert!(gate.current().is_some());
    assert!(gate.verified_attachment().is_none());
    assert!(host_grants.attachment().is_none());
    assert!(matches!(
        host_grants.room(RoomBinding::Room(&room)).await,
        RoomGate::Unavailable
    ));

    let mut owner = fixture.server.client("owner", "phone").await;
    let refused = join(&mut owner, &room, "owner").await;
    assert!(
        error_condition(&refused).is_some(),
        "an owner joined while the authority was refused: {refused:?}"
    );
    let reply = round_trip(
        &mut romeo,
        set("mam", room.as_str(), mam_query("q-refused")),
    )
    .await;
    assert!(
        !is_result(&reply),
        "the archive was read while the authority was refused: {reply:?}"
    );

    // The lineage is put back: the gate's retry passes the fence and the
    // surfaces decide the room again.
    sqlx::query("UPDATE _lineage SET lineage_uuid = $1 WHERE id = 1")
        .bind(&lineage)
        .execute(&pool)
        .await
        .expect("lineage restored");
    pool.close().await;
    gate_reaches(&fixture, GateState::Attached, Duration::from_secs(30)).await;
    assert!(host_grants.attachment().is_some());
    assert!(matches!(
        host_grants.room(RoomBinding::Room(&room)).await,
        RoomGate::Managed(_)
    ));
    let mut owner = fixture.server.client("owner", "laptop").await;
    let joined = join(&mut owner, &room, "owner").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "an owner's join after the fence passed: {joined:?}"
    );
    let reply = round_trip(&mut romeo, set("mam", room.as_str(), mam_query("q-after"))).await;
    assert!(is_result(&reply), "a member's archive read: {reply:?}");
    fixture.close().await;
}

#[tokio::test]
async fn administration_is_recorded_in_the_store_and_writes_no_relationship() {
    let Some(fixture) = ManagedRoom::start("hg_fence_admin").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut owner = fixture.server.client("owner", "phone").await;
    let joined = join(&mut owner, &room, "owner").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "the owner's join: {joined:?}"
    );

    // An increase is recorded and waits for its projection; no legacy
    // relationship is written for it.
    let reply = round_trip(
        &mut owner,
        set("grant", room.as_str(), admin_item("member", CAROL)),
    )
    .await;
    assert!(is_result(&reply), "the owner's grant: {reply:?}");
    assert_eq!(
        fixture.grants.affiliations(CAROL).await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );
    assert!(fixture.legacy_relations(CAROL).await.is_empty());

    // A ban is effective at its commit.
    let reply = round_trip(
        &mut owner,
        set("ban", room.as_str(), admin_item("outcast", PEER)),
    )
    .await;
    assert!(is_result(&reply), "the owner's ban: {reply:?}");
    assert_eq!(
        fixture.grants.affiliations(PEER).await,
        Some((RoomAffiliation::Outcast, RoomAffiliation::Outcast))
    );
    assert!(fixture.legacy_relations(PEER).await.is_empty());

    // A privileged grant to an account of a home that is not trusted is
    // refused before anything is recorded (ADR-0020 D8).
    let reply = round_trip(
        &mut owner,
        set("remote", room.as_str(), admin_item("admin", REMOTE)),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::NotAllowed),
        "a remote admin grant: {reply:?}"
    );
    assert_eq!(fixture.grants.affiliations(REMOTE).await, None);

    // A batch is refused: one change per request.
    let batch = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    "member",
                )
                .attr(
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    "dave@example.com",
                )
                .build(),
        )
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    "member",
                )
                .attr(
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    "erin@example.com",
                )
                .build(),
        )
        .build();
    let reply = round_trip(&mut owner, set("batch", room.as_str(), batch)).await;
    assert!(
        is_error(&reply, DefinedCondition::NotAcceptable),
        "a batch: {reply:?}"
    );

    // A member administers nothing, whatever the room actor's list says.
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );
    fixture
        .mirror_affiliation(SENDER, waddle_xmpp::Affiliation::Owner)
        .await;
    let reply = round_trip(
        &mut romeo,
        set("member-ban", room.as_str(), admin_item("outcast", OWNER)),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a member's ban: {reply:?}"
    );
    assert_eq!(
        fixture.grants.affiliations(OWNER).await,
        Some((RoomAffiliation::Owner, RoomAffiliation::Owner))
    );

    // Owner configuration needs the canonical manage action, and the room
    // is never destroyed through it.
    let reply = round_trip(&mut romeo, get("config", room.as_str(), owner_query())).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a member's configuration read: {reply:?}"
    );
    let reply = round_trip(&mut owner, get("config", room.as_str(), owner_query())).await;
    assert!(
        is_result(&reply),
        "the owner's configuration read: {reply:?}"
    );
    let reply = round_trip(&mut owner, set("destroy", room.as_str(), owner_destroy())).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "the owner destroyed a managed room: {reply:?}"
    );

    // The affiliation lists come from the store.
    let list = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    "outcast",
                )
                .build(),
        )
        .build();
    let reply = round_trip(&mut owner, get("outcasts", room.as_str(), list)).await;
    let listed: Vec<String> = match &reply {
        xmpp_parsers::iq::Iq::Result {
            payload: Some(query),
            ..
        } => query
            .children()
            .filter_map(|item| item.attr("jid").map(str::to_owned))
            .collect(),
        other => panic!("the outcast list was refused: {other:?}"),
    };
    assert_eq!(listed, vec![PEER.to_owned()]);
    fixture.close().await;
}

#[tokio::test]
async fn invitations_and_space_pubsub_grant_nothing_in_an_enrolled_scope() {
    let Some(fixture) = ManagedRoom::start("hg_fence_invite").await else {
        return;
    };
    let room = fixture.room().clone();
    let mut romeo = fixture.server.client("romeo", "phone").await;
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(
        error_condition(&joined),
        None,
        "a member's join: {joined:?}"
    );

    let invite = Element::builder("message", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room.as_str())
        .append(
            Element::builder("x", NS_MUC_USER)
                .append(
                    Element::builder("invite", NS_MUC_USER)
                        .attr(minidom::rxml::xml_ncname!("to").to_owned(), CAROL)
                        .build(),
                )
                .build(),
        )
        .build();
    romeo
        .send(&String::from(&invite))
        .await
        .expect("send invite");
    let reply = romeo
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "message" && element.attr("type") == Some("error")
            })
        })
        .await
        .expect("invitation refusal");
    let reply: Element = reply.parse().expect("message XML");
    assert_eq!(error_condition(&reply).as_deref(), Some("not-allowed"));
    assert_eq!(fixture.grants.affiliations(CAROL).await, None);

    // The scope's Space node is not served through PubSub.
    let node = fixture.grants.scope.space_node.clone();
    let reply = round_trip(&mut romeo, get("items", SPACES, pubsub_items(&node))).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "items: {reply:?}"
    );
    let reply = round_trip(
        &mut romeo,
        set("subscribe", SPACES, pubsub_subscribe(&node, SENDER)),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "subscribe: {reply:?}"
    );
    let bookmark = waddle_xmpp::xep::xep0402::build_bookmark_element(
        &waddle_xmpp::xep::xep0402::Bookmark::new(room.clone()),
    );
    let reply = round_trip(
        &mut romeo,
        set(
            "publish",
            SPACES,
            pubsub_publish(&node, room.as_str(), bookmark),
        ),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "publish: {reply:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn legacy_reads_of_an_enrolled_channel_or_space_answer_nothing() {
    let Some(fixture) = GrantFixture::active("hg_fence_reads").await else {
        return;
    };
    let db = Arc::new(fixture.ingress.db.clone());
    let channel = Object::new(ObjectType::Channel, fixture.scope.channel_id.as_str());
    let space = Object::new(ObjectType::Space, fixture.scope.space_node.as_str());
    let other = Object::new(ObjectType::Channel, "unmanaged-room");

    // Relationships a stale mirror or an old binary left behind.
    let unguarded = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::clone(&db)));
    for object in [&channel, &space, &other] {
        unguarded
            .ask(WriteTuple {
                tuple: Tuple::new(
                    object.clone(),
                    Relation::new("member"),
                    Subject::user(CAROL),
                ),
            })
            .await
            .expect("legacy relationship");
    }
    let guarded = PermissionActor::spawn(
        PermissionActor::new_for_tests(db).with_host_grant_store(fixture.store.clone()),
    );
    for object in [&channel, &space] {
        let check = guarded
            .ask(CheckPermission {
                subject: Subject::user(CAROL),
                permission: Permission::Member,
                object: object.clone(),
            })
            .await
            .expect("check");
        assert!(
            !check.allowed,
            "a legacy check of an enrolled object answered"
        );
        let relations = guarded
            .ask(ListRelations {
                subject: Subject::user(CAROL),
                object: object.clone(),
            })
            .await
            .expect("relations");
        assert!(relations.is_empty());
        let subjects = guarded
            .ask(ListSubjects {
                object: object.clone(),
                relation: Relation::new("member"),
            })
            .await
            .expect("subjects");
        assert!(subjects.is_empty());
    }
    // A room no scope manages keeps its legacy answers.
    let check = guarded
        .ask(CheckPermission {
            subject: Subject::user(CAROL),
            permission: Permission::Member,
            object: other,
        })
        .await
        .expect("check");
    assert!(check.allowed);
    fixture.close().await;
}

#[tokio::test]
async fn durable_membership_of_an_enrolled_room_is_its_admitted_grants() {
    let Some(fixture) = GrantFixture::active("hg_fence_durable").await else {
        return;
    };
    let db = Arc::new(fixture.ingress.db.clone());
    let actor = PermissionActor::spawn(PermissionActor::new_for_tests(db));
    // A legacy Space member and a legacy channel member hold no grant.
    for object in [
        Object::new(ObjectType::Channel, fixture.scope.channel_id.as_str()),
        Object::new(ObjectType::Space, "space"),
    ] {
        actor
            .ask(WriteTuple {
                tuple: Tuple::new(object, Relation::new("member"), Subject::user(CAROL)),
            })
            .await
            .expect("legacy membership");
    }
    let gate = HostGrantGate::new(fixture.ingress.db.clone());
    let unattached =
        PermissionDurableMembershipSource::new(actor.clone()).with_host_grants(gate.clone());
    assert!(
        unattached
            .list_durable_member_jids("space", &fixture.scope.channel_id)
            .await
            .is_err(),
        "an unattached replica named a managed room's members"
    );
    assert!(gate.attach(
        HostGrantAttachment::unfenced_for_test(gate.store().expect("store"), served())
            .await
            .expect("attachment")
    ));
    let source = PermissionDurableMembershipSource::new(actor).with_host_grants(gate);
    let members = source
        .list_durable_member_jids("space", &fixture.scope.channel_id)
        .await
        .expect("durable members");
    assert_eq!(members, vec![bare(PEER), bare(OWNER), bare(SENDER)]);

    fixture
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    let members = source
        .list_durable_member_jids("space", &fixture.scope.channel_id)
        .await
        .expect("durable members");
    assert_eq!(members, vec![bare(OWNER), bare(SENDER)]);
    fixture.close().await;
}

#[tokio::test]
async fn owner_reseed_keeps_bans_skips_enrolled_spaces_and_refuses_remote_accounts() {
    let Some(fixture) = GrantFixture::active("hg_fence_reseed").await else {
        return;
    };
    let gate = HostGrantGate::new(fixture.ingress.db.clone());
    let spaces: BareJid = SPACES.parse().expect("Spaces service");
    let enrolled = fixture.scope.space_node.clone();
    let banned = bare("mallory@example.com");
    let owner = bare(OWNER);
    let remote = bare(REMOTE);
    let trusted = bare("frank@trusted.example");

    let sql = DatabasePubSubStorage::open(None)
        .await
        .expect("SQL PubSub storage");
    let stores: [Arc<dyn PubSubStorage>; 2] =
        [Arc::new(InMemoryPubSubStorage::new()), Arc::new(sql)];
    for storage in stores {
        for node in ["plain", enrolled.as_str()] {
            storage
                .get_or_create_node(&spaces, node)
                .await
                .expect("Space node");
        }
        storage
            .set_affiliation(&spaces, "plain", &banned, Affiliation::Outcast)
            .await
            .expect("ban");

        let issuance = issuance_policy(&[]);
        let policy = OwnerSeedPolicy {
            issuance: &issuance,
            host_grants: &gate,
        };
        let outcomes = seed_owners_on_node(
            &storage,
            &spaces,
            "plain",
            &[
                banned.clone(),
                owner.clone(),
                remote.clone(),
                trusted.clone(),
            ],
            policy,
        )
        .await;
        assert_eq!(
            outcomes,
            vec![
                OwnerSeedOutcome::KeptOutcast,
                OwnerSeedOutcome::Seeded,
                OwnerSeedOutcome::RefusedSubject,
                OwnerSeedOutcome::RefusedSubject,
            ]
        );
        assert_eq!(
            storage
                .affiliation(&spaces, "plain", &banned)
                .await
                .expect("read"),
            Affiliation::Outcast
        );
        assert_eq!(
            storage
                .affiliation(&spaces, "plain", &remote)
                .await
                .expect("read"),
            Affiliation::None
        );

        // The Space node of an enrolled scope is never seeded.
        let outcomes = seed_owners_on_node(
            &storage,
            &spaces,
            &enrolled,
            std::slice::from_ref(&owner),
            policy,
        )
        .await;
        assert_eq!(outcomes, vec![OwnerSeedOutcome::RefusedNode]);
        assert_eq!(
            storage
                .affiliation(&spaces, &enrolled, &owner)
                .await
                .expect("read"),
            Affiliation::None
        );
        let outcomes = seed_owner_on_all_nodes(&storage, &spaces, &banned, policy)
            .await
            .expect("seed every node");
        assert!(!outcomes.contains(&OwnerSeedOutcome::Seeded));

        // A trusted home the seed is given may hold a privileged grant.
        let with_home = issuance_policy(&["trusted.example"]);
        let outcomes = seed_owners_on_node(
            &storage,
            &spaces,
            "plain",
            std::slice::from_ref(&trusted),
            OwnerSeedPolicy {
                issuance: &with_home,
                host_grants: &gate,
            },
        )
        .await;
        assert_eq!(outcomes, vec![OwnerSeedOutcome::Seeded]);
    }
    fixture.close().await;
}
