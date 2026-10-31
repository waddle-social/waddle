//! W1.6a characterization suite (#2046): resource binding and the
//! registration it drives, pinned exactly as the server behaves today.
//!
//! Every case runs over a real loopback WebSocket (RFC 7395) against an
//! in-process server: the production `/ws` router from
//! `waddle_server::test_support::router` serving the default test state, and
//! the shared SCRAM client from `waddle-ws-test-support`. Server frames are
//! parsed into `xmpp_parsers` types before any assertion.
//!
//! What a bind does today (`websocket/frame.rs` → `resource_binding.rs` →
//! `registration.rs`): the bind IQ only moves the stream phase to Ready; the
//! answer is written after `register_bound_connection_after_frame` has taken
//! the per-full-JID bind gate, retired any incumbent, published the
//! `ConnectionRegistry` route and asked the `UserRegistryActor` to register
//! the resource (`RegisterUserResource`, which acquires or validates the
//! `user_actor` claim inside the registry's single node-wide turn and then
//! asks the `UserActor` with `RegisterConnection`). A failed registration
//! never answers the bind; it closes the stream instead.
//!
//! Fault seams (test-only, no production code):
//! - `GatedClaimStore` below is a `ClaimStore` that delegates to
//!   `InProcessClaimStore` and can park `ensure_claimed`/`fence` for one bare
//!   JID. It is installed with the public `WireUserClusteringClaims` message,
//!   the same entry point production uses to swap in the clustered store.
//! - `GateMailbox` (`waddle-xmpp` `test-utils`) parks a `UserActor` mailbox,
//!   so a `RegisterConnection` queues behind it.
//! - The `ConnectionRegistry` `test-utils` pending-bind observer signals only
//!   after a competing per-JID lock future actually polls to `Pending`.
//!
//! These suites need neither Postgres nor `clustering`; they need the
//! `test-support` feature, so they run in the all-features publish lane and
//! are listed in `server/.config/required-cluster-tests.txt`.

#![cfg(feature = "test-support")]

use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, FullJid, Jid};
use kameo::error::SendError;
use minidom::Element;
use tokio::sync::{oneshot, watch, Notify};
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::test_support::{router, seed_local_account, websocket_state, WebSocketState};
use waddle_ws_test_support::raw_ws::Message as WsFrame;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, ExactReleaseOutcome,
    InProcessClaimStore, NodeIdentity, ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
};
use waddle_xmpp::registry::user_actor::test_support::GateMailbox;
use waddle_xmpp::registry::{GetResources, GetUser, UserRegistryError, WireUserClusteringClaims};
use xmpp_parsers::bind::{BindQuery, BindResponse};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition as StanzaCondition, ErrorType};
use xmpp_parsers::stream_error::{DefinedCondition as StreamCondition, StreamError};

/// The test state pins `xmpp_domain` to `example.com` (`test_state.rs`).
const DOMAIN: &str = "example.com";
const PASSWORD: &str = "characterization-password";
/// Failure detector for one expected server frame. Generous because a busy
/// registration legitimately spends the 2 s child-actor budget first.
const FRAME_BOUND: Duration = Duration::from_secs(20);
/// Failure detector for a condition that the test polls (registry state).
const CONDITION_BOUND: Duration = Duration::from_secs(20);
const CONDITION_POLL: Duration = Duration::from_millis(5);

// ---------------------------------------------------------------------------
// In-process server and clients
// ---------------------------------------------------------------------------

struct Harness {
    state: Arc<WebSocketState>,
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn start() -> Harness {
    let state = websocket_state().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("listener address");
    let app = router(Arc::clone(&state));
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Harness {
        state,
        url: format!("ws://{addr}/ws"),
        server,
    }
}

impl Harness {
    async fn add_account(&self, localpart: &str) {
        seed_local_account(&self.state, localpart).await;
        NativeUserStore::new(self.state.deps.app_state.db_pool.global_actor().clone())
            .register(RegisterRequest {
                username: localpart.to_owned(),
                domain: DOMAIN.to_owned(),
                password: PASSWORD.to_owned(),
                email: None,
            })
            .await
            .expect("register native SCRAM account");
    }

    /// Open a stream and finish SASL SCRAM-SHA-256; the stream is ready for
    /// resource binding (RFC 6120 §7) and nothing has touched the registries.
    async fn authenticated(&self, localpart: &str) -> WsXmppClient {
        let mut client = WsXmppClient::connect(&self.url)
            .await
            .expect("WebSocket connect");
        client
            .authenticate(DOMAIN, localpart, PASSWORD)
            .await
            .expect("SCRAM-SHA-256 authentication");
        client
    }

    fn is_routed(&self, jid: &FullJid) -> bool {
        self.state
            .deps
            .protocol
            .connection_registry
            .is_connected(jid)
    }

    fn registry_mailbox_capacity(&self) -> usize {
        self.state
            .deps
            .protocol
            .user_registry
            .mailbox_sender()
            .capacity()
            .expect("user registry has a bounded mailbox")
    }

    async fn wait_for_queued_registrations(
        &self,
        capacity_before: usize,
        expected_queued: usize,
        seam: &ClaimSeam,
    ) {
        let expected_capacity = capacity_before
            .checked_sub(expected_queued)
            .expect("registry has room for the expected registrations");
        tokio::time::timeout(CONDITION_BOUND, async {
            while self.registry_mailbox_capacity() != expected_capacity {
                tokio::time::sleep(CONDITION_POLL).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "registrations not admitted within {CONDITION_BOUND:?}: \
                 capacity_before={capacity_before}, expected_queued={expected_queued}, \
                 capacity_now={}, registry_alive={}, claim_events={:?}",
                self.registry_mailbox_capacity(),
                self.state.deps.protocol.user_registry.is_alive(),
                seam.events(),
            )
        });
    }

    /// The resources the account's `UserActor` holds, sorted.
    async fn actor_resources(&self, bare: &BareJid) -> Vec<FullJid> {
        let Some(actor) = self
            .state
            .deps
            .protocol
            .user_registry
            .ask(GetUser {
                bare_jid: bare.clone(),
            })
            .await
            .expect("registry GetUser")
        else {
            return Vec::new();
        };
        let mut resources = actor.ask(GetResources).await.expect("UserActor resources");
        resources.sort();
        resources
    }

    async fn wire_claim_store(&self, store: GatedClaimStore) {
        self.state
            .deps
            .protocol
            .user_registry
            .ask(WireUserClusteringClaims {
                claim_store: Arc::new(store),
                node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            })
            .await
            .expect("wire the gated claim store into the user registry");
    }
}

async fn within<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(FRAME_BOUND, future)
        .await
        .unwrap_or_else(|_| panic!("{what} not observed within {FRAME_BOUND:?}"))
}

fn full(text: &str) -> FullJid {
    text.parse().expect("full JID")
}

fn bare(text: &str) -> BareJid {
    text.parse().expect("bare JID")
}

fn serialize(element: Element) -> String {
    String::from(&element)
}

async fn send_bind(client: &mut WsXmppClient, id: &str, resource: Option<&str>) {
    let iq = Iq::from_set(id, BindQuery::new(resource.map(str::to_owned)));
    client
        .send(&serialize(iq.into()))
        .await
        .expect("send bind IQ");
}

async fn send_chat(client: &mut WsXmppClient, to: &FullJid, body: &str) {
    let mut message = Message::new_with_type(MessageType::Chat, Some(Jid::from(to.clone())))
        .with_body(Lang::default(), body.to_owned());
    message.id = Some(Id(uuid::Uuid::new_v4().to_string()));
    client
        .send(&serialize(message.into()))
        .await
        .expect("send chat message");
}

/// One server-to-client event, parsed once.
#[derive(Debug)]
enum ServerEvent {
    Frame(Element),
    /// WebSocket Close frame or end of stream.
    Closed,
}

async fn next_event(client: &mut WsXmppClient) -> ServerEvent {
    loop {
        match client
            .recv_raw_timeout(FRAME_BOUND)
            .await
            .expect("server frame within bound")
        {
            Some(WsFrame::Text(text)) => {
                return ServerEvent::Frame(
                    Element::from_str(text.as_str()).expect("server frame is well-formed XML"),
                )
            }
            Some(WsFrame::Close(_)) | None => return ServerEvent::Closed,
            Some(WsFrame::Ping(_) | WsFrame::Pong(_)) => continue,
            Some(other) => panic!("unexpected WebSocket frame: {other:?}"),
        }
    }
}

async fn next_frame(client: &mut WsXmppClient) -> Element {
    match next_event(client).await {
        ServerEvent::Frame(frame) => frame,
        ServerEvent::Closed => panic!("stream closed while a frame was expected"),
    }
}

/// RFC 6120 §7.6.1 / §7.7.1: `<iq type='result'>` carrying `<bind><jid/>`.
fn bind_result(frame: &Element, id: &str) -> Option<FullJid> {
    match Iq::try_from(frame.clone()).ok()? {
        Iq::Result {
            id: result_id,
            payload: Some(payload),
            ..
        } if result_id == id => BindResponse::try_from(payload).ok().map(FullJid::from),
        _ => None,
    }
}

/// RFC 6120 §4.9: `<stream:error/>` condition.
fn stream_error(frame: &Element) -> Option<StreamCondition> {
    if !frame.is("error", waddle_xmpp::ns::STREAM) {
        return None;
    }
    Some(
        StreamError::try_from(frame.clone())
            .expect("typed stream error")
            .condition,
    )
}

/// RFC 7395 §3.6: the framing `<close/>` element.
fn is_framing_close(frame: &Element) -> bool {
    frame.is("close", xmpp_parsers::ns::WEBSOCKET)
}

async fn expect_bind_result(client: &mut WsXmppClient, id: &str) -> FullJid {
    let frame = within("bind result", next_frame(client)).await;
    bind_result(&frame, id).unwrap_or_else(|| panic!("expected bind result {id}, got {frame:?}"))
}

/// Read until the WebSocket closes and return every XMPP frame on the way.
async fn frames_until_closed(client: &mut WsXmppClient) -> Vec<Element> {
    let mut frames = Vec::new();
    tokio::time::timeout(FRAME_BOUND, async {
        loop {
            match next_event(client).await {
                ServerEvent::Frame(frame) => frames.push(frame),
                ServerEvent::Closed => return,
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("stream close not observed within {FRAME_BOUND:?}; frames: {frames:?}")
    });
    frames
}

/// The two frames RFC 7395 §3.5/§3.6 put on the wire for a stream error:
/// the `<stream:error/>` and then the framing `<close/>`.
fn assert_stream_error_then_close(frames: &[Element], condition: StreamCondition, who: &str) {
    assert_eq!(
        frames.len(),
        2,
        "{who}: exactly a stream error and a framing close, got {frames:?}"
    );
    assert_eq!(
        stream_error(&frames[0]),
        Some(condition),
        "{who}: stream error condition, got {:?}",
        frames[0]
    );
    assert!(
        is_framing_close(&frames[1]),
        "{who}: framing close follows the stream error, got {:?}",
        frames[1]
    );
}

async fn expect_chat(client: &mut WsXmppClient, from: &FullJid, body: &str) {
    let frame = within("routed chat message", next_frame(client)).await;
    let message = Message::try_from(frame.clone())
        .unwrap_or_else(|_| panic!("expected a routed <message/>, got {frame:?}"));
    assert_eq!(message.type_, MessageType::Chat);
    assert_eq!(message.from, Some(Jid::from(from.clone())));
    assert_eq!(
        message.get_best_body(vec![]).map(|(_, text)| text.as_str()),
        Some(body)
    );
}

// ---------------------------------------------------------------------------
// Claim-store seam
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaimOp {
    EnsureSchema,
    Acquire,
    EnsureClaimed,
    StealStale,
    StealForResume,
    CurrentClaim,
    CurrentClaimAfterPendingWrites,
    Fence,
    Release,
    ReleaseExact,
    ReleaseMany,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaimEvent {
    Entered(ClaimOp, String),
    Returned(ClaimOp, String),
    /// The seam engaged: the call found its gate closed and is parked.
    Held(ClaimOp, String),
    /// The gate opened and the parked call proceeds to the real store.
    Released(ClaimOp, String),
    /// The caller dropped the call before it returned (its own timeout).
    Abandoned(ClaimOp, String),
}

type ClaimLog = Arc<Mutex<Vec<ClaimEvent>>>;

struct CallRecord {
    log: ClaimLog,
    op: ClaimOp,
    entity: String,
    returned: bool,
}

impl CallRecord {
    fn enter(log: &ClaimLog, op: ClaimOp, entity: String) -> Self {
        log.lock()
            .expect("claim log")
            .push(ClaimEvent::Entered(op, entity.clone()));
        Self {
            log: Arc::clone(log),
            op,
            entity,
            returned: false,
        }
    }

    fn returned(mut self) {
        self.returned = true;
        self.log
            .lock()
            .expect("claim log")
            .push(ClaimEvent::Returned(self.op, self.entity.clone()));
    }
}

impl Drop for CallRecord {
    fn drop(&mut self) {
        if !self.returned {
            if let Ok(mut log) = self.log.lock() {
                log.push(ClaimEvent::Abandoned(self.op, self.entity.clone()));
            }
        }
    }
}

/// Opens a gate for the rest of the test (dropping the sender also opens it).
struct GateHandle(watch::Sender<bool>);

impl GateHandle {
    fn open(&self) {
        let _ = self.0.send(true);
    }
}

fn gate(open: bool) -> (GateHandle, watch::Receiver<bool>) {
    let (tx, rx) = watch::channel(open);
    (GateHandle(tx), rx)
}

async fn wait_open(mut gate: watch::Receiver<bool>) {
    let _ = gate.wait_for(|open| *open).await;
}

/// Test-only `ClaimStore`: delegates to `InProcessClaimStore` (the default
/// single-node store) and logs every call. For one `user_actor` entity it
/// parks `ensure_claimed` and `fence` until their gates open — a claim
/// store that is slow for that account, without a sleep.
struct GatedClaimStore {
    inner: InProcessClaimStore,
    gated: String,
    acquire_gate: watch::Receiver<bool>,
    fence_gate: watch::Receiver<bool>,
    acquire_entered: Arc<Notify>,
    log: ClaimLog,
}

impl GatedClaimStore {
    async fn observe<T>(&self, op: ClaimOp, entity: &Entity, call: impl Future<Output = T>) -> T {
        let record = CallRecord::enter(&self.log, op, entity.id.clone());
        if entity.entity_type == EntityType::UserActor && entity.id == self.gated {
            let gate = match op {
                ClaimOp::EnsureClaimed => Some(self.acquire_gate.clone()),
                ClaimOp::Fence => Some(self.fence_gate.clone()),
                _ => None,
            };
            if let Some(gate) = gate {
                if !*gate.borrow() {
                    self.push(ClaimEvent::Held(op, entity.id.clone()));
                    if op == ClaimOp::EnsureClaimed {
                        self.acquire_entered.notify_one();
                    }
                    wait_open(gate).await;
                    self.push(ClaimEvent::Released(op, entity.id.clone()));
                }
            }
        }
        let result = call.await;
        record.returned();
        result
    }

    fn push(&self, event: ClaimEvent) {
        self.log.lock().expect("claim log").push(event);
    }
}

#[async_trait]
impl ClaimStore for GatedClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        let record = CallRecord::enter(&self.log, ClaimOp::EnsureSchema, String::new());
        let result = self.inner.ensure_schema().await;
        record.returned();
        result
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        self.observe(ClaimOp::Acquire, entity, self.inner.acquire(entity, me))
            .await
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.observe(
            ClaimOp::EnsureClaimed,
            entity,
            self.inner.ensure_claimed(entity, me),
        )
        .await
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.observe(
            ClaimOp::StealStale,
            entity,
            self.inner.steal_stale(entity, observed, staleness, me),
        )
        .await
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.observe(
            ClaimOp::StealForResume,
            entity,
            self.inner.steal_for_resume(entity, observed, witness, me),
        )
        .await
    }

    async fn current_claim(&self, entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.observe(
            ClaimOp::CurrentClaim,
            entity,
            self.inner.current_claim(entity),
        )
        .await
    }

    async fn current_claim_after_pending_writes(
        &self,
        entity: &Entity,
    ) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.observe(
            ClaimOp::CurrentClaimAfterPendingWrites,
            entity,
            self.inner.current_claim_after_pending_writes(entity),
        )
        .await
    }

    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        self.observe(ClaimOp::Fence, entity, self.inner.fence(entity, me, mine))
            .await
    }

    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        self.observe(
            ClaimOp::Release,
            entity,
            self.inner.release(entity, me, mine),
        )
        .await
    }

    async fn release_exact(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<ExactReleaseOutcome, ClaimError> {
        self.observe(
            ClaimOp::ReleaseExact,
            entity,
            self.inner.release_exact(entity, me, mine),
        )
        .await
    }

    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        let record = CallRecord::enter(&self.log, ClaimOp::ReleaseMany, String::new());
        let result = self.inner.release_many(entities, me).await;
        record.returned();
        result
    }
}

struct ClaimSeam {
    store: Option<GatedClaimStore>,
    acquire: GateHandle,
    fence: GateHandle,
    acquire_entered: Arc<Notify>,
    log: ClaimLog,
}

/// A seam for `gated`'s `user_actor` claim: `ensure_claimed` parked until
/// [`ClaimSeam::acquire`] opens, `fence` parked unless `fence_open`.
fn claim_seam(gated: &BareJid, fence_open: bool) -> ClaimSeam {
    let (acquire, acquire_gate) = gate(false);
    let (fence, fence_gate) = gate(fence_open);
    let acquire_entered = Arc::new(Notify::new());
    let log = ClaimLog::default();
    ClaimSeam {
        store: Some(GatedClaimStore {
            inner: InProcessClaimStore::new(),
            gated: gated.to_string(),
            acquire_gate,
            fence_gate,
            acquire_entered: Arc::clone(&acquire_entered),
            log: Arc::clone(&log),
        }),
        acquire,
        fence,
        acquire_entered,
        log,
    }
}

impl ClaimSeam {
    fn events(&self) -> Vec<ClaimEvent> {
        self.log.lock().expect("claim log").clone()
    }
}

fn position(events: &[ClaimEvent], wanted: &ClaimEvent) -> Option<usize> {
    events.iter().position(|event| event == wanted)
}

fn count(events: &[ClaimEvent], wanted: &ClaimEvent) -> usize {
    events.iter().filter(|event| *event == wanted).count()
}

// ---------------------------------------------------------------------------
// Bind and rebind
// ---------------------------------------------------------------------------

/// RFC 6120 §7.7.1: a client-submitted resource is accepted as requested,
/// and the answer is sent only after both the `ConnectionRegistry` route and
/// the `UserActor` resource exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_bind_returns_requested_full_jid_and_registers_route_and_user_actor() {
    let harness = start().await;
    harness.add_account("alice").await;
    let phone = full("alice@example.com/phone");

    let mut client = harness.authenticated("alice").await;
    send_bind(&mut client, "bind-phone", Some("phone")).await;

    assert_eq!(expect_bind_result(&mut client, "bind-phone").await, phone);
    assert!(
        harness.is_routed(&phone),
        "route published before the answer"
    );
    assert_eq!(
        harness.actor_resources(&phone.to_bare()).await,
        vec![phone.clone()],
        "UserActor registered the resource before the answer"
    );
}

/// RFC 6120 §7.6.1: an empty `<bind/>` gets a server-generated resource;
/// today that resource is `ws-` followed by a v4 UUID.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_without_resource_assigns_server_generated_ws_resource() {
    let harness = start().await;
    harness.add_account("alice").await;

    let mut client = harness.authenticated("alice").await;
    send_bind(&mut client, "bind-generated", None).await;

    let bound = expect_bind_result(&mut client, "bind-generated").await;
    assert_eq!(bound.to_bare(), bare("alice@example.com"));
    let generated = bound
        .resource()
        .as_str()
        .strip_prefix("ws-")
        .unwrap_or_else(|| panic!("server-generated resource starts with ws-: {bound}"));
    let uuid = uuid::Uuid::parse_str(generated).expect("ws- suffix is a UUID");
    assert_eq!(uuid.get_version_num(), 4);
    assert!(harness.is_routed(&bound));
}

/// A second `<bind/>` on an already-bound stream is answered with an IQ
/// error `auth`/`<not-authorized/>` (RFC 6120 §8.3.3.11; the phase gate
/// only admits binding while `Authenticated`), the original binding is
/// untouched and the requested resource is never registered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_bind_on_bound_stream_is_refused_not_authorized_and_keeps_first_binding() {
    let harness = start().await;
    harness.add_account("alice").await;
    let phone = full("alice@example.com/phone");
    let laptop = full("alice@example.com/laptop");

    let mut client = harness.authenticated("alice").await;
    send_bind(&mut client, "bind-phone", Some("phone")).await;
    assert_eq!(expect_bind_result(&mut client, "bind-phone").await, phone);

    send_bind(&mut client, "bind-laptop", Some("laptop")).await;
    let frame = within("second bind answer", next_frame(&mut client)).await;
    match Iq::try_from(frame.clone()).expect("typed IQ") {
        Iq::Error { id, error, .. } => {
            assert_eq!(id, "bind-laptop");
            assert_eq!(error.type_, ErrorType::Auth);
            assert_eq!(error.defined_condition, StanzaCondition::NotAuthorized);
        }
        other => panic!("expected an IQ error for the second bind, got {other:?}"),
    }

    assert!(harness.is_routed(&phone));
    assert!(!harness.is_routed(&laptop));
    assert_eq!(
        harness.actor_resources(&phone.to_bare()).await,
        vec![phone.clone()]
    );
}

/// RFC 6120 §7.7.2.2 option (3): a new stream binding a full JID that a live
/// stream holds terminates the incumbent. The incumbent gets
/// `<stream:error><conflict/>` (§4.9.3.3) and the framing `<close/>`
/// (RFC 7395 §3.6) and its WebSocket closes; the newcomer's bind succeeds and
/// the route delivers to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebind_of_live_full_jid_closes_incumbent_with_conflict_and_routes_to_newcomer() {
    let harness = start().await;
    harness.add_account("alice").await;
    harness.add_account("bob").await;
    let phone = full("alice@example.com/phone");
    let desk = full("bob@example.com/desk");

    let mut bob = harness.authenticated("bob").await;
    send_bind(&mut bob, "bind-desk", Some("desk")).await;
    assert_eq!(expect_bind_result(&mut bob, "bind-desk").await, desk);

    let mut incumbent = harness.authenticated("alice").await;
    send_bind(&mut incumbent, "bind-old", Some("phone")).await;
    assert_eq!(expect_bind_result(&mut incumbent, "bind-old").await, phone);

    let mut newcomer = harness.authenticated("alice").await;
    send_bind(&mut newcomer, "bind-new", Some("phone")).await;
    assert_eq!(expect_bind_result(&mut newcomer, "bind-new").await, phone);

    let incumbent_frames = frames_until_closed(&mut incumbent).await;
    assert_stream_error_then_close(&incumbent_frames, StreamCondition::Conflict, "incumbent");

    assert!(harness.is_routed(&phone));
    assert_eq!(
        harness.actor_resources(&phone.to_bare()).await,
        vec![phone.clone()]
    );
    send_chat(&mut bob, &phone, "after rebind").await;
    expect_chat(&mut newcomer, &desk, "after rebind").await;
}

/// Once the previous stream for a full JID has closed and both its route and
/// its `UserActor` resource are gone, binding it again is an ordinary fresh bind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebind_after_clean_close_binds_without_conflict() {
    let harness = start().await;
    harness.add_account("alice").await;
    let phone = full("alice@example.com/phone");

    let mut first = harness.authenticated("alice").await;
    send_bind(&mut first, "bind-first", Some("phone")).await;
    assert_eq!(expect_bind_result(&mut first, "bind-first").await, phone);
    let actor = within("original UserActor lookup", async {
        harness
            .state
            .deps
            .protocol
            .user_registry
            .ask(GetUser {
                bare_jid: phone.to_bare(),
            })
            .await
    })
    .await
    .expect("registry GetUser")
    .expect("original UserActor exists after binding");
    assert_eq!(
        within("original UserActor resources", async {
            actor.ask(GetResources).await
        })
        .await
        .expect("UserActor resources"),
        vec![phone.clone()]
    );
    first.close().await.expect("RFC 7395 close handshake");
    // Route removal precedes the asynchronous mirror unregister. Query the
    // original actor directly: a missing/replaced registry entry must not
    // masquerade as successful cleanup of that actor's resource.
    let mut last_resources = None;
    tokio::time::timeout(CONDITION_BOUND, async {
        loop {
            let resources = actor
                .ask(GetResources)
                .await
                .expect("original UserActor remains queryable during clean close");
            let cleaned = resources.is_empty() && !harness.is_routed(&phone);
            last_resources = Some(resources);
            if cleaned {
                return;
            }
            tokio::time::sleep(CONDITION_POLL).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "clean-close cleanup not observed within {CONDITION_BOUND:?}: \
             jid={phone}, routed={}, original_actor_alive={}, last_resources={last_resources:?}",
            harness.is_routed(&phone),
            actor.is_alive(),
        )
    });

    let mut second = harness.authenticated("alice").await;
    send_bind(&mut second, "bind-second", Some("phone")).await;
    assert_eq!(expect_bind_result(&mut second, "bind-second").await, phone);
    assert_eq!(
        harness.actor_resources(&phone.to_bare()).await,
        vec![phone.clone()]
    );
}

#[derive(Default, Debug)]
struct Transcript {
    frames: Vec<Element>,
    closed: bool,
}

impl Transcript {
    fn record(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::Frame(frame) => self.frames.push(frame),
            ServerEvent::Closed => self.closed = true,
        }
    }

    fn bound(&self, id: &str) -> bool {
        self.frames
            .iter()
            .any(|frame| bind_result(frame, id).is_some())
    }
}

/// Two streams of one account overlap while binding the same full JID. The
/// first is parked in claim acquisition while holding the per-full-JID gate;
/// the second must poll that gate's lock future to `Pending` before the first
/// is released. The second then retires the first and stays live holding the
/// route; the first ends with `<conflict/>` and `<close/>` (RFC 6120
/// §4.9.3.3, RFC 7395 §3.6), possibly after its own bind result, because the
/// retirement's write interruption can land before or after that result is
/// written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_same_full_jid_binds_leave_one_live_session_and_conflict_close_the_other() {
    let harness = start().await;
    harness.add_account("alice").await;
    harness.add_account("bob").await;
    let phone = full("alice@example.com/phone");
    let desk = full("bob@example.com/desk");
    let mut seam = claim_seam(&phone.to_bare(), true);
    harness
        .wire_claim_store(seam.store.take().expect("store"))
        .await;

    let mut bob = harness.authenticated("bob").await;
    send_bind(&mut bob, "bind-desk", Some("desk")).await;
    assert_eq!(expect_bind_result(&mut bob, "bind-desk").await, desk);

    let mut first = harness.authenticated("alice").await;
    let mut second = harness.authenticated("alice").await;
    send_bind(&mut first, "bind-first", Some("phone")).await;
    within(
        "first bind's claim acquisition to park",
        seam.acquire_entered.notified(),
    )
    .await;
    let pending = harness
        .state
        .deps
        .protocol
        .connection_registry
        .observe_next_pending_bind(&phone);
    send_bind(&mut second, "bind-second", Some("phone")).await;
    within("second bind to wait on the held per-JID gate", pending)
        .await
        .expect("bind contention observer remains installed until Pending");
    seam.acquire.open();

    let mut first_transcript = Transcript::default();
    let mut second_transcript = Transcript::default();
    within("one survivor and one conflict-closed stream", async {
        loop {
            let settled = (first_transcript.closed && second_transcript.bound("bind-second"))
                || (second_transcript.closed && first_transcript.bound("bind-first"));
            if settled {
                return;
            }
            tokio::select! {
                event = next_event(&mut first), if !first_transcript.closed => {
                    first_transcript.record(event);
                }
                event = next_event(&mut second), if !second_transcript.closed => {
                    second_transcript.record(event);
                }
            }
        }
    })
    .await;

    assert!(
        first_transcript.closed,
        "the parked first binder is retired: {first_transcript:?}"
    );
    assert!(
        !second_transcript.closed,
        "the waiting second binder survives: {second_transcript:?}"
    );
    let (mut survivor, survivor_transcript, survivor_id, loser_transcript, loser_id) = (
        second,
        second_transcript,
        "bind-second",
        first_transcript,
        "bind-first",
    );

    assert_eq!(
        survivor_transcript.frames.len(),
        1,
        "survivor sees only its bind result: {survivor_transcript:?}"
    );
    assert_eq!(
        bind_result(&survivor_transcript.frames[0], survivor_id),
        Some(phone.clone())
    );

    let loser_tail = match loser_transcript.frames.first() {
        Some(frame) if bind_result(frame, loser_id).is_some() => {
            assert_eq!(bind_result(frame, loser_id), Some(phone.clone()));
            &loser_transcript.frames[1..]
        }
        _ => &loser_transcript.frames[..],
    };
    assert_stream_error_then_close(loser_tail, StreamCondition::Conflict, "loser");

    assert!(harness.is_routed(&phone));
    assert_eq!(
        harness.actor_resources(&phone.to_bare()).await,
        vec![phone.clone()]
    );
    send_chat(&mut bob, &phone, "to the survivor").await;
    expect_chat(&mut survivor, &desk, "to the survivor").await;
}

// ---------------------------------------------------------------------------
// Claim store and registration delays
// ---------------------------------------------------------------------------

/// Characterizes current behaviour; changes with E0 (#2046).
///
/// Two first binds of one account (alice/phone, alice/laptop) and a bind of
/// an unrelated account (bob/desk) while alice's `user_actor` claim
/// acquisition is parked in the claim store. Today the claim I/O runs inside
/// `UserRegistryActor`'s single node-wide turn (`acquire_user_claim` from
/// `RegisterUserResource`), so nothing else touches the claim store until it
/// returns: laptop waits and then reuses the actor phone created (one
/// acquisition, a `fence` validation), and bob's own acquisition starts only
/// after alice's returned. Every bind is answered once the claim is
/// released. E0 moves the claim I/O off the registry turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_first_binds_serialize_node_wide_behind_parked_user_claim_acquire() {
    let harness = start().await;
    harness.add_account("alice").await;
    harness.add_account("bob").await;
    let alice = bare("alice@example.com");
    let phone = full("alice@example.com/phone");
    let laptop = full("alice@example.com/laptop");
    let desk = full("bob@example.com/desk");
    let mut seam = claim_seam(&alice, true);
    harness
        .wire_claim_store(seam.store.take().expect("store"))
        .await;

    let mut phone_client = harness.authenticated("alice").await;
    let mut laptop_client = harness.authenticated("alice").await;
    let mut bob_client = harness.authenticated("bob").await;

    send_bind(&mut phone_client, "bind-phone", Some("phone")).await;
    within(
        "alice's claim acquisition to park",
        seam.acquire_entered.notified(),
    )
    .await;
    let capacity_before = harness.registry_mailbox_capacity();
    send_bind(&mut laptop_client, "bind-laptop", Some("laptop")).await;
    send_bind(&mut bob_client, "bind-desk", Some("desk")).await;
    // The parked registry turn cannot consume either request. The mailbox
    // capacity delta proves both registration asks were admitted before release.
    harness
        .wait_for_queued_registrations(capacity_before, 2, &seam)
        .await;
    seam.acquire.open();

    assert_eq!(
        expect_bind_result(&mut phone_client, "bind-phone").await,
        phone
    );
    assert_eq!(
        expect_bind_result(&mut laptop_client, "bind-laptop").await,
        laptop
    );
    assert_eq!(expect_bind_result(&mut bob_client, "bind-desk").await, desk);

    let events = seam.events();
    let alice_id = alice.to_string();
    let bob_id = bare("bob@example.com").to_string();
    let alice_acquire = ClaimEvent::Entered(ClaimOp::EnsureClaimed, alice_id.clone());
    let parked_at = position(&events, &alice_acquire).expect("alice's acquisition");
    assert_eq!(
        events.get(parked_at + 1..parked_at + 4),
        Some(
            &[
                ClaimEvent::Held(ClaimOp::EnsureClaimed, alice_id.clone()),
                ClaimEvent::Released(ClaimOp::EnsureClaimed, alice_id.clone()),
                ClaimEvent::Returned(ClaimOp::EnsureClaimed, alice_id.clone()),
            ][..]
        ),
        "the seam held alice's acquisition and no claim-store call of any \
         account ran until it returned: {events:?}"
    );
    assert_eq!(
        count(&events, &alice_acquire),
        1,
        "one acquisition serves both of alice's resources: {events:?}"
    );
    let laptop_validation = position(
        &events,
        &ClaimEvent::Entered(ClaimOp::Fence, alice_id.clone()),
    )
    .expect("laptop validated the actor phone created");
    assert!(laptop_validation > parked_at + 3, "{events:?}");
    let bob_acquire = ClaimEvent::Entered(ClaimOp::EnsureClaimed, bob_id);
    assert_eq!(count(&events, &bob_acquire), 1, "{events:?}");
    assert!(
        position(&events, &bob_acquire).expect("bob's acquisition") > parked_at + 3,
        "bob's acquisition waited for alice's: {events:?}"
    );

    assert_eq!(
        harness.actor_resources(&alice).await,
        vec![laptop.clone(), phone.clone()]
    );
    assert_eq!(
        harness.actor_resources(&desk.to_bare()).await,
        vec![desk.clone()]
    );
}

/// Characterizes current behaviour; changes with E0 (#2046).
///
/// Against a claim store that is slow for alice, two concurrent first binds:
/// phone's acquisition waits as long as the store takes (no bound inside the
/// registry turn), but laptop, queued behind it, validates the now-existing
/// actor with `fence`, which the registry bounds at 250 ms
/// (`CLAIM_RELEASE_TIMEOUT`). The validation is abandoned, the registry
/// answers `ClaimUnavailable`, the bind maps it to `Busy` and laptop's stream
/// ends with `<resource-constraint/>` (RFC 6120 §4.9.3.17) and `<close/>`
/// without a bind answer; its route is rolled back. Phone stays bound, and a
/// retry once the store is fast again binds laptop. E0 moves this claim
/// I/O (`user_registry.rs:1083-1088`, the existing-actor validation
/// included) off the registry turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_claim_fence_fails_concurrent_second_resource_bind_with_resource_constraint() {
    let harness = start().await;
    harness.add_account("alice").await;
    let alice = bare("alice@example.com");
    let phone = full("alice@example.com/phone");
    let laptop = full("alice@example.com/laptop");
    let mut seam = claim_seam(&alice, false);
    harness
        .wire_claim_store(seam.store.take().expect("store"))
        .await;

    let mut phone_client = harness.authenticated("alice").await;
    let mut laptop_client = harness.authenticated("alice").await;

    send_bind(&mut phone_client, "bind-phone", Some("phone")).await;
    within(
        "alice's claim acquisition to park",
        seam.acquire_entered.notified(),
    )
    .await;
    let capacity_before = harness.registry_mailbox_capacity();
    send_bind(&mut laptop_client, "bind-laptop", Some("laptop")).await;
    harness
        .wait_for_queued_registrations(capacity_before, 1, &seam)
        .await;
    seam.acquire.open();

    assert_eq!(
        expect_bind_result(&mut phone_client, "bind-phone").await,
        phone
    );
    let laptop_frames = frames_until_closed(&mut laptop_client).await;
    assert_stream_error_then_close(
        &laptop_frames,
        StreamCondition::ResourceConstraint,
        "laptop",
    );

    let events = seam.events();
    let alice_id = alice.to_string();
    let fence_entered = position(
        &events,
        &ClaimEvent::Entered(ClaimOp::Fence, alice_id.clone()),
    )
    .expect("laptop validated the actor phone created");
    assert_eq!(
        events.get(fence_entered + 1..fence_entered + 3),
        Some(
            &[
                ClaimEvent::Held(ClaimOp::Fence, alice_id.clone()),
                ClaimEvent::Abandoned(ClaimOp::Fence, alice_id.clone()),
            ][..]
        ),
        "the seam held laptop's fence and the registry gave up on it: {events:?}"
    );
    let acquire = ClaimEvent::Entered(ClaimOp::EnsureClaimed, alice_id.clone());
    assert_eq!(count(&events, &acquire), 1, "{events:?}");
    let acquired_at = position(&events, &acquire).expect("phone's acquisition");
    assert_eq!(
        events.get(acquired_at + 1..acquired_at + 4),
        Some(
            &[
                ClaimEvent::Held(ClaimOp::EnsureClaimed, alice_id.clone()),
                ClaimEvent::Released(ClaimOp::EnsureClaimed, alice_id.clone()),
                ClaimEvent::Returned(ClaimOp::EnsureClaimed, alice_id.clone()),
            ][..]
        ),
        "the seam held phone's acquisition until laptop was queued: {events:?}"
    );
    assert!(fence_entered > acquired_at + 3, "{events:?}");
    assert!(!harness.is_routed(&laptop), "laptop's route rolled back");
    assert!(harness.is_routed(&phone));
    // Every registry lookup of an existing account validates its claim the
    // same way, so while the store stays slow the lookup fails too.
    let lookup = harness
        .state
        .deps
        .protocol
        .user_registry
        .ask(GetUser {
            bare_jid: alice.clone(),
        })
        .await;
    assert!(
        matches!(
            lookup,
            Err(SendError::HandlerError(UserRegistryError::ClaimUnavailable(ref jid))) if *jid == alice
        ),
        "GetUser while the fence is slow: {lookup:?}"
    );

    seam.fence.open();
    assert_eq!(harness.actor_resources(&alice).await, vec![phone.clone()]);
    let mut retry = harness.authenticated("alice").await;
    send_bind(&mut retry, "bind-laptop-retry", Some("laptop")).await;
    assert_eq!(
        expect_bind_result(&mut retry, "bind-laptop-retry").await,
        laptop
    );
    assert_eq!(
        harness.actor_resources(&alice).await,
        vec![laptop.clone(), phone.clone()]
    );
}

/// Characterizes current behaviour; changes with E0 (#2046).
///
/// This router-only harness omits the production critical-registry watch.
/// E0's row names this W1.6 case: it moves the `RegisterConnection` ask
/// (`user_registry.rs:1090-1097`) out of the registry turn and orders
/// register/unregister completions per resource, so this path changes there.
/// The registry stopping when a `tell` handler returns `Err` is a defect this
/// suite found; no §3 row names it yet.
///
/// alice/desk is bound, then alice's `UserActor` mailbox is parked so the
/// `RegisterConnection` for alice/phone queues behind it. The registry's 2 s
/// child budget (`CHILD_ACTOR_TIMEOUT`) expires, the bind maps
/// `UserActorBusy` to `Busy`, phone's stream ends with
/// `<resource-constraint/>` and `<close/>` (RFC 6120 §4.9.3.17) and the bind
/// rolls back: route removed, then `mirror_unregister` sends an owner-gated
/// `UnregisterUserResource` as a kameo `tell`. That turn asks the still
/// parked `UserActor` and times out too, so the handler returns
/// `Err(UserActorBusy)`; kameo treats an `Err` reply to a `tell` as an actor
/// failure and the default `on_panic` stops the node-wide
/// `UserRegistryActor` (vendor/kameo/src/message.rs:410-418,
/// actor/kind.rs:168-170). Messages queued behind it fail with
/// `ActorStopped`. This harness does not start the production critical-registry
/// lifetime watch (`spawn_user_registry_lifetime_watch`), which marks the node
/// failed and requests process stop when the registry terminates. Subsequent
/// binds here therefore characterize the surviving router only, not production
/// behaviour after that watch fires: they fail registration with
/// `<internal-server-error/>` (RFC 6120 §4.9.3.8) and `<close/>`. The
/// `UserActor` in this harness is not linked to the registry and survives: when its
/// mailbox resumes it runs the late `RegisterConnection` and then the queued
/// owner-gated unregister, so it ends with exactly desk (no phantom phone).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_then_unregister_behind_a_busy_user_actor_stops_the_user_registry() {
    let harness = start().await;
    harness.add_account("alice").await;
    harness.add_account("bob").await;
    let alice = bare("alice@example.com");
    let desk = full("alice@example.com/desk");
    let phone = full("alice@example.com/phone");
    let laptop = full("bob@example.com/laptop");
    let registry = &harness.state.deps.protocol.user_registry;

    let mut desk_client = harness.authenticated("alice").await;
    send_bind(&mut desk_client, "bind-desk", Some("desk")).await;
    assert_eq!(
        expect_bind_result(&mut desk_client, "bind-desk").await,
        desk
    );

    let actor = registry
        .ask(GetUser {
            bare_jid: alice.clone(),
        })
        .await
        .expect("registry GetUser")
        .expect("alice's UserActor");
    let parked = Arc::new(Notify::new());
    let (release, release_rx) = oneshot::channel();
    actor
        .tell(GateMailbox {
            entered: Arc::clone(&parked),
            release_rx,
        })
        .await
        .expect("park alice's UserActor mailbox");
    within("UserActor mailbox to park", parked.notified()).await;

    let mut phone_client = harness.authenticated("alice").await;
    send_bind(&mut phone_client, "bind-phone", Some("phone")).await;
    let phone_frames = frames_until_closed(&mut phone_client).await;
    assert_stream_error_then_close(&phone_frames, StreamCondition::ResourceConstraint, "phone");
    assert!(!harness.is_routed(&phone), "phone's route rolled back");

    // FIFO barrier: the rollback enqueued its unregister tell before the
    // stream error was written, so this ask is answered (or dropped) only
    // after that turn ends.
    let lookup = registry
        .ask(GetUser {
            bare_jid: alice.clone(),
        })
        .await;
    assert!(
        matches!(
            lookup,
            Err(SendError::ActorStopped | SendError::ActorNotRunning(_))
        ),
        "the registry stopped after its unregister tell returned Err: {lookup:?}"
    );
    assert!(!registry.is_alive(), "UserRegistryActor stopped");

    // `send` succeeds only while the parked `GateMailbox` handler still waits
    // on the receiver: the seam held the mailbox through the whole sequence.
    release
        .send(())
        .expect("alice's UserActor mailbox was still parked by the seam");
    let mut resources = actor.ask(GetResources).await.expect("UserActor resources");
    resources.sort();
    assert_eq!(resources, vec![desk.clone()], "no phantom phone resource");
    assert!(harness.is_routed(&desk), "desk's route is untouched");

    let mut bob_client = harness.authenticated("bob").await;
    send_bind(&mut bob_client, "bind-laptop", Some("laptop")).await;
    let bob_frames = frames_until_closed(&mut bob_client).await;
    assert_stream_error_then_close(&bob_frames, StreamCondition::InternalServerError, "bob");
    assert!(!harness.is_routed(&laptop), "bob's route rolled back");
}
