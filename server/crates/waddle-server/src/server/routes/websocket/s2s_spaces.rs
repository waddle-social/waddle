//! The Spaces of this deployment as accounts at other servers use them
//! (ADR-0020 D10 and D4; XEP-0503 version 0.3.0 over XEP-0060, with
//! XEP-0030 and XEP-0199).
//!
//! The S2S router hands each IQ request a remote principal addressed to the
//! Spaces PubSub service to [`WebSocketLocalSpaces`], which decides it for
//! that principal on the evidence of the stream that carried it
//! ([`PrincipalEvidence::of`]) and answers with typed stanzas. Nothing here
//! is a session: a remote principal holds no account and no connection on
//! this deployment. Each request is decided in this order:
//!
//! 1. Peer admission (D1), under the S2S snapshot the replica runs: a
//!    domain it does not admit, or a stream below what host policy
//!    requires, is refused whatever a Space holds for the account.
//! 2. What the request itself says: a request that is not well formed, or
//!    that names an address other than the sender's own to subscribe, is
//!    refused the same way whatever node it names.
//! 3. Whether the node is a Space open to this asker ([`Asked::space`]): it
//!    exists, its `federated` setting is on, no enrolled host-grant scope
//!    owns it, and it is public or the asker holds an affiliation in it
//!    that is honoured for its stream. Any other node is answered as one
//!    that does not exist ([`no_such_node`]), in every operation alike.
//! 4. The operation, for an open Space.
//!
//! A PubSub affiliation of a remote principal is honoured as the effect
//! boundary honours any standing (D8, [`honoured`]): a ban always, a
//! membership while the domain is admitted, and the owner and publisher
//! affiliations only for a trusted home on a `pkix` stream, where one that
//! is not honoured contributes nothing at all. Every operation that changes
//! a Space needs honoured privilege first, and is then decided by the
//! checks a session's request meets.
//!
//! What a Space lists is its rooms. A remote principal is told of a room
//! only where the room's own host would describe it to that principal
//! ([`Asked::child_visible`]): its `federated` setting is on, and, for a
//! room an enrolled host-grant scope manages, its grants admit the
//! principal to view it on that evidence, behind the revocation fence. A
//! subscription to a Space is no grant in any of those rooms: entering one
//! is a join of the room, decided by the room.
//!
//! A Space may also hold its avatar and banner items (XEP-0503 0.3.0,
//! "Spaces images"). They are no rooms: a principal that may read the
//! Space reads them with its items and is notified of them, while the
//! Space's `disco#items` lists its rooms alone. Only an item whose payload
//! is a Space image's metadata ([`validate_space_image_metadata`]) is
//! served; the image URLs it names are never fetched.
//!
//! A Space's publications are handed to [`WebSocketLocalSpaces`] once they
//! are stored, within the Space's allowance of notified publications, on a
//! bounded queue, and a task of its own sends their event notifications to
//! the subscribers at other servers ([`notify_remote_subscribers`]), each
//! only for a subscription the remote account made itself and decided on
//! the evidence that subscription recorded. The publisher's request waits
//! for none of it.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Instant;

use async_trait::async_trait;
use jid::{BareJid, Jid};
use minidom::Element;
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::pubsub::{
    build_pubsub_error, build_pubsub_items_result, build_pubsub_subscribe_result,
    build_pubsub_success, parse_pubsub_iq, AccessModel, Affiliation, PubSubError, PubSubItem,
    PubSubNode, PubSubRequest, SubId, NS_PUBSUB, NS_PUBSUB_ERRORS, NS_PUBSUB_OWNER,
};
use waddle_xmpp::xep::build_spaces_metadata_form_for_remote;
use waddle_xmpp::xep::xep0402::NS_BOOKMARKS2;
use waddle_xmpp::xep::xep0503::{validate_space_image_metadata, SpaceImage};
use waddle_xmpp::Stanza;
use xmpp_parsers::disco::{
    DiscoInfoQuery, DiscoInfoResult, DiscoItemsQuery, DiscoItemsResult, Feature, Identity,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::rsm::SetQuery;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::handlers::iq::{page_listing, spaces_change_for_remote};
use super::handlers::presence::principal_witness;
use super::WebSocketState;
use crate::db::s2s_spaces::{
    FederatedSpaces, RemovedSubscription, SpaceSubscriptions, SubscriptionRecord,
    SubscriptionRequest,
};
use crate::db::Database;
use crate::permissions::host_grant::{
    HostGrantAction, PrincipalEvidence, PrincipalWitness, RoomBinding, RoomGate, ScopeKey,
};
use crate::s2s::route::SpacePublication;
use crate::s2s::route::{LocalSpaces, RemotePrincipal, SpaceOperation, SpaceReply, SpaceRequest};
use crate::space_identity::SpaceNode;

/// The field of a Space's configuration form that says whether accounts at
/// other servers may reach the Space: a boolean, off unless its owner turns
/// it on.
pub const FIELD_SPACE_FEDERATED: &str = "urn:waddle:spaces:federated";

/// The most Spaces one `disco#items` answer lists to a remote asker.
const MAX_LISTED_SPACES: usize = 256;

/// The most items one answer returns to a remote asker.
const MAX_RETURNED_ITEMS: u32 = 256;

/// Publications of one Space, on average each second, queued to be decided
/// for subscribers at other servers.
const EVENT_RATE: f64 = 4.0;

/// Publications of one Space at once queued to be decided for subscribers
/// at other servers.
const EVENT_BURST: f64 = 16.0;

/// Spaces whose allowance of notified publications is kept. A Space that
/// finds the table full of Spaces still spending theirs sends none to other
/// servers, so the table never grows with the Spaces that publish.
const EVENT_ALLOWANCES: usize = 4096;

/// Publications stored in local Spaces and not yet decided for their
/// subscribers at other servers. A publication is queued only within its
/// Space's allowance, so one Space holds at most [`EVENT_BURST`] places
/// at once; one that finds the queue full is not notified to them, so a
/// burst of publications never holds a publisher and never grows the
/// replica's memory.
const PUBLICATION_QUEUE: usize = 1024;

/// What one Space may still have notified to other servers.
struct EventAllowance {
    tokens: f64,
    refilled: Instant,
}

/// The allowances of notified publications of the Spaces of one host,
/// taken as each publication is handed over, before it is queued.
#[derive(Default)]
struct EventBudget {
    allowances: HashMap<String, EventAllowance>,
}

impl EventBudget {
    /// Take one publication of the allowance of publications notified to
    /// other servers that the Space `node` of `service` has: `false` when
    /// it is spent or the table has no place for the Space.
    fn take(&mut self, service: &BareJid, node: &str) -> bool {
        let node = [service.as_str(), "/", node].concat();
        let now = Instant::now();
        let allowances = &mut self.allowances;
        if allowances.len() >= EVENT_ALLOWANCES && !allowances.contains_key(&node) {
            // A Space whose allowance has refilled needs no entry.
            allowances.retain(|_, allowance| {
                now.duration_since(allowance.refilled).as_secs_f64() * EVENT_RATE
                    < EVENT_BURST - allowance.tokens
            });
            if allowances.len() >= EVENT_ALLOWANCES {
                return false;
            }
        }
        let allowance = allowances.entry(node).or_insert(EventAllowance {
            tokens: EVENT_BURST,
            refilled: now,
        });
        let refill = now.duration_since(allowance.refilled).as_secs_f64() * EVENT_RATE;
        allowance.tokens = (allowance.tokens + refill).min(EVENT_BURST);
        allowance.refilled = now;
        if allowance.tokens >= 1.0 {
            allowance.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// What the notification task of a host is handed.
enum Queued {
    /// A stored publication to decide for the Space's remote subscribers.
    Publication(Box<SpacePublication>),
    /// Answered once everything queued before it has been decided.
    Settled(tokio::sync::oneshot::Sender<()>),
}

mod telemetry {
    use waddle_xmpp::counter_add;

    /// A request reached the Spaces host from a domain the replica's S2S
    /// snapshot does not admit, or over a stream below what host policy
    /// requires.
    pub(super) fn not_admitted() {
        counter_add!(
            "waddle.s2s.spaces.requests.not_admitted",
            "{stanza}",
            "Requests from S2S streams that reached the Spaces host from a domain the replica's S2S snapshot does not admit, or over a stream below what host policy requires, refused with forbidden.",
            1,
        );
    }

    /// A remote account's subscription to a local Space was refused for a
    /// bound on such subscriptions.
    pub(super) fn subscription_limited() {
        counter_add!(
            "waddle.s2s.spaces.subscriptions.limited",
            "{stanza}",
            "Subscriptions of remote accounts to local Spaces refused with resource-constraint because a bound on the Space's remote subscribers, on those of one domain in the Space, on the account's or its domain's subscriptions or on the deployment's was reached.",
            1,
        );
    }

    /// A publication of a local Space was not handed to the task that
    /// notifies its subscribers at other servers: the queue was full.
    pub(super) fn publication_dropped() {
        counter_add!(
            "waddle.s2s.spaces.publications.dropped",
            "{event}",
            "Publications of local Spaces whose event notifications were not sent to subscribers at other servers because the queue of publications awaiting that decision was full.",
            1,
        );
    }

    /// Publications of local Spaces queued for their subscribers at other
    /// servers and never decided: the replica's state, or its Spaces host,
    /// was going away.
    pub(super) fn publications_discarded(count: u64) {
        counter_add!(
            "waddle.s2s.spaces.publications.discarded",
            "{event}",
            "Publications of local Spaces queued for their subscribers at other servers that were never decided because the replica's state or its Spaces host was shutting down.",
            count,
        );
    }

    /// A publication of a local Space was not queued for its subscribers at
    /// other servers: the Space had spent its allowance of notified
    /// publications.
    pub(super) fn publication_limited() {
        counter_add!(
            "waddle.s2s.spaces.events.limited",
            "{event}",
            "Publications of local Spaces not queued for subscribers at other servers because the Space had spent its allowance of notified publications, counted whether or not the Space has such subscribers.",
            1,
        );
    }

    /// An event notification was withheld from one subscriber at another
    /// server: its subscription is no longer deliverable or honoured, or it
    /// may not learn of the room the event names.
    pub(super) fn event_withheld() {
        counter_add!(
            "waddle.s2s.spaces.events.withheld",
            "{stanza}",
            "Event notifications of local Spaces withheld from a subscriber at another server because its subscription is no longer deliverable or honoured, or it may not learn of the room the event names.",
            1,
        );
    }
}

/// [`LocalSpaces`] over the WebSocket state's PubSub storage and grants.
///
/// It holds the state weakly: the state holds the router, which holds this.
/// Its notification task holds the state weakly too, and ends once the
/// state is gone or this host is dropped.
pub struct WebSocketLocalSpaces {
    state: Weak<WebSocketState>,
    publications: tokio::sync::mpsc::Sender<Queued>,
    /// The Spaces' allowances of queued publications. A publisher takes
    /// from it as it hands a publication over, so the queue holds no more
    /// of one Space's than its allowance.
    budget: std::sync::Mutex<EventBudget>,
}

impl WebSocketLocalSpaces {
    /// A host of `state`'s Spaces, with its notification task started on
    /// the current Tokio runtime.
    pub fn new(state: &Arc<WebSocketState>) -> Self {
        let (publications, queued) = tokio::sync::mpsc::channel(PUBLICATION_QUEUE);
        tokio::spawn(notify_publications(Arc::downgrade(state), queued));
        Self {
            state: Arc::downgrade(state),
            publications,
            budget: std::sync::Mutex::new(EventBudget::default()),
        }
    }

    /// Take one publication of the Space's allowance; `false` when it is
    /// spent, or the allowances cannot be read.
    fn within_allowance(&self, publication: &SpacePublication) -> bool {
        let Ok(mut budget) = self.budget.lock() else {
            return false;
        };
        budget.take(&publication.service, &publication.node)
    }
}

/// The notification task of one host: each publication taken, in the order
/// taken, decided for the Space's subscribers at other servers. Once the
/// state is gone, what is still queued is discarded and counted.
async fn notify_publications(
    state: Weak<WebSocketState>,
    mut queued: tokio::sync::mpsc::Receiver<Queued>,
) {
    while let Some(next) = queued.recv().await {
        match next {
            Queued::Publication(publication) => {
                let Some(state) = state.upgrade() else {
                    discard_queued(&mut queued);
                    return;
                };
                notify_remote_subscribers(&state, &publication).await;
            }
            Queued::Settled(settled) => {
                let _ = settled.send(());
            }
        }
    }
}

/// Close `queued` and count the publication in hand and every one still in
/// it as discarded; whoever waits for the queue to settle is answered.
fn discard_queued(queued: &mut tokio::sync::mpsc::Receiver<Queued>) {
    queued.close();
    let mut discarded: u64 = 1;
    while let Ok(next) = queued.try_recv() {
        match next {
            Queued::Publication(_) => discarded += 1,
            Queued::Settled(settled) => {
                let _ = settled.send(());
            }
        }
    }
    telemetry::publications_discarded(discarded);
}

/// Have `router` serve the Spaces of `state` to remote principals: the
/// router hands what they ask of the Spaces PubSub service to
/// [`WebSocketLocalSpaces`], and takes the event notifications of those
/// Spaces for subscribers at other servers.
pub(crate) fn host_spaces(state: &Arc<WebSocketState>, router: &crate::s2s::route::S2sRouter) {
    router.attach_spaces(Arc::new(WebSocketLocalSpaces::new(state)));
}

fn error(type_: ErrorType, condition: DefinedCondition, text: &str) -> StanzaError {
    StanzaError::new(type_, condition, "en", text)
}

fn forbidden(text: &str) -> StanzaError {
    error(ErrorType::Auth, DefinedCondition::Forbidden, text)
}

fn unavailable() -> StanzaError {
    error(
        ErrorType::Cancel,
        DefinedCondition::ServiceUnavailable,
        "The request is not available to entities at other servers.",
    )
}

fn bad_request() -> StanzaError {
    error(
        ErrorType::Modify,
        DefinedCondition::BadRequest,
        "The request is not well formed.",
    )
}

/// The answer for a node a remote asker may not learn about: one that does
/// not exist, one whose `federated` setting is off, the Space of an
/// enrolled host-grant scope, and a private Space in which the asker holds
/// no honoured affiliation. They are answered alike, in condition, type
/// and text and with no application condition, so the answer says nothing
/// about which.
fn no_such_node() -> StanzaError {
    error(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "Requested item not found.",
    )
}

fn internal() -> StanzaError {
    error(
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        "The service could not answer now; try again later.",
    )
}

/// XEP-0060 §6.2.3.2: an `<unsubscribe/>` from an account that holds no
/// subscription to the node, with the PubSub condition `<not-subscribed/>`.
fn not_subscribed() -> StanzaError {
    let mut error = error(
        ErrorType::Cancel,
        DefinedCondition::UnexpectedRequest,
        "You are not subscribed to this Space.",
    );
    error.other = Some(Element::builder("not-subscribed", NS_PUBSUB_ERRORS).build());
    error
}

fn busy() -> StanzaError {
    error(
        ErrorType::Wait,
        DefinedCondition::ResourceConstraint,
        "Too many subscriptions; try again later.",
    )
}

/// The error answering the request `iq`, from the address it was sent to.
fn refuse(iq: &Iq, error: StanzaError) -> Vec<Stanza> {
    vec![Stanza::Iq(Box::new(Iq::Error {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_owned(),
        payload: None,
        error,
    }))]
}

/// The result answering the request `iq`, from the address it was sent to.
fn result(iq: &Iq, payload: Option<Element>) -> Vec<Stanza> {
    vec![Stanza::Iq(Box::new(Iq::Result {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_owned(),
        payload,
    }))]
}

fn answer(iq: Iq) -> Vec<Stanza> {
    vec![Stanza::Iq(Box::new(iq))]
}

/// What a PubSub affiliation `stored` for a remote principal is honoured as
/// under `witness` (ADR-0020 D8). A ban always stands. A membership counts
/// while the principal's grants are honoured at all. The owner and
/// publisher affiliations, which administer a Space and place rooms in it,
/// count only where privilege is honoured: a trusted home on a `pkix`
/// stream. An affiliation that is not honoured contributes nothing, not
/// the membership it would otherwise imply.
fn honoured(witness: &PrincipalWitness, stored: Affiliation) -> Affiliation {
    match stored {
        Affiliation::Outcast => Affiliation::Outcast,
        Affiliation::Member if witness.grants_honoured() => Affiliation::Member,
        Affiliation::Owner | Affiliation::Publisher | Affiliation::PublishOnly
            if witness.privilege_honoured() =>
        {
            stored
        }
        Affiliation::Owner
        | Affiliation::Publisher
        | Affiliation::PublishOnly
        | Affiliation::Member
        | Affiliation::None => Affiliation::None,
    }
}

/// Whether an honoured `affiliation` lets its holder read and subscribe to
/// a Space of `access`: anybody who is not banned in a public Space, and
/// the Space's members, publishers and owners in a private one. A
/// publish-only affiliation reads nothing (XEP-0060 §4.1).
fn reads(access: AccessModel, affiliation: Affiliation) -> bool {
    match (access, affiliation) {
        (_, Affiliation::Outcast) => false,
        (AccessModel::Open, _) => true,
        (
            AccessModel::Whitelist,
            Affiliation::Owner | Affiliation::Publisher | Affiliation::Member,
        ) => true,
        (AccessModel::Whitelist, Affiliation::PublishOnly | Affiliation::None) => false,
        (AccessModel::Presence | AccessModel::Roster | AccessModel::Authorize, _) => false,
    }
}

/// Whether a node is a Space open to the asker cannot be told now: the
/// request is answered with a retryable error.
struct Undecided;

/// A Space a remote asker is answered about.
struct OpenSpace {
    node: PubSubNode,
    /// The asker's affiliation in it, as honoured for its stream.
    affiliation: Affiliation,
}

impl OpenSpace {
    fn created_at_ms(&self) -> i64 {
        self.node.created_at.timestamp_millis()
    }
}

/// One request, as its handlers read it.
struct Asked<'a> {
    state: &'a Arc<WebSocketState>,
    iq: &'a Iq,
    /// The Spaces service's address.
    service: BareJid,
    /// The sender's bare JID: the subject of every decision.
    account: BareJid,
    /// What the stream that carried the request proved.
    evidence: PrincipalEvidence,
    witness: PrincipalWitness,
    /// The S2S snapshot that stream was admitted under.
    snapshot: uuid::Uuid,
}

fn database(state: &WebSocketState) -> &Database {
    state.deps.app_state.db_pool.global()
}

/// Whether the Space `node` of `service` is open to other servers: its
/// `federated` setting is on for this very node. `None` when the setting
/// cannot be read.
pub(crate) async fn space_federated(
    state: &WebSocketState,
    service: &BareJid,
    node: &PubSubNode,
) -> Option<bool> {
    FederatedSpaces::new(database(state).clone())
        .is_federated(service, &node.node_name, node.created_at.timestamp_millis())
        .await
        .ok()
}

/// Set the `federated` setting of the Space `node` of `service`, created at
/// `node_created_at_ms`. Turning it off forgets the subscriptions remote
/// accounts made to the Space, in the transaction that turns it off, and
/// then removes the service's own subscription records of accounts this
/// deployment does not serve, so nothing more leaves for them and nothing
/// of theirs is left behind. `false` when the setting could not be written
/// or those records could not all be removed.
pub(crate) async fn set_space_federated(
    state: &WebSocketState,
    service: &BareJid,
    node: &str,
    node_created_at_ms: i64,
    federated: bool,
) -> bool {
    let db = database(state);
    if federated {
        let now_ms = chrono::Utc::now().timestamp_millis();
        return matches!(
            FederatedSpaces::new(db.clone())
                .enable(service, node, node_created_at_ms, now_ms)
                .await,
            Ok(true)
        );
    }
    if close_space(db, service, node).await.is_err() {
        return false;
    }
    remove_remote_pubsub_subscriptions(state, service, node).await
}

/// Close the Space `node` of `service` to other servers: its `federated`
/// setting and the subscriptions remote accounts made to it are removed in
/// one transaction ([`FederatedSpaces::close`]).
pub(crate) async fn close_space(
    db: &Database,
    service: &BareJid,
    node: &str,
) -> Result<(), crate::db::s2s_spaces::SpaceStoreError> {
    FederatedSpaces::new(db.clone()).close(service, node).await
}

/// Remove the PubSub service's subscription records of `node` held for
/// addresses this deployment does not serve: they were made by remote
/// accounts, or recorded for one by an owner, and nothing is sent for them
/// once the Space is closed to other servers. `false` when one could not be
/// read or removed.
async fn remove_remote_pubsub_subscriptions(
    state: &WebSocketState,
    service: &BareJid,
    node: &str,
) -> bool {
    remove_remote_subscription_records(
        &state.deps.protocol.pubsub_storage,
        service,
        node,
        &|address| state.serves(address),
    )
    .await
}

/// Remove the subscription records `storage` holds for `node` of `service`
/// for addresses `serves` does not answer for: those of accounts at other
/// servers. `false` when one could not be read or removed.
pub(crate) async fn remove_remote_subscription_records(
    storage: &Arc<dyn waddle_xmpp::pubsub::PubSubStorage>,
    service: &BareJid,
    node: &str,
    serves: &(dyn Fn(&BareJid) -> bool + Send + Sync),
) -> bool {
    let Ok(subscriptions) = storage.list_node_subscriptions(service, node).await else {
        return false;
    };
    let mut removed = true;
    for subscription in subscriptions {
        if serves(&subscription.subscriber.to_bare()) {
            continue;
        }
        removed &= storage
            .unsubscribe(
                service,
                node,
                &subscription.subscriber,
                Some(&subscription.subid),
            )
            .await
            .is_ok();
    }
    removed
}

/// Forget the subscription the remote `account` made to the Space `node`
/// of `service`: its owner removed the service's subscription record of
/// the account, or banned it. `false` when the row could not be removed.
pub(crate) async fn forget_remote_subscription(
    state: &WebSocketState,
    service: &BareJid,
    node: &str,
    account: &BareJid,
) -> bool {
    if state.serves(account) {
        return true;
    }
    SpaceSubscriptions::new(database(state).clone())
        .remove(service, node, account)
        .await
        .is_ok()
}

/// Whether the domain of a remote account is admitted now by the S2S
/// snapshot the replica runs (ADR-0020 D1): never while S2S is off.
fn admitted_now(state: &WebSocketState) -> impl Fn(&str) -> bool + Send + Sync + '_ {
    let federation = state.deps.app_state.s2s_federation.as_ref();
    move |domain| {
        federation.is_some_and(|federation| {
            waddle_xmpp::identity::CanonicalDomain::parse_canonical(domain)
                .is_ok_and(|domain| federation.admission().decide(&domain).is_ok())
        })
    }
}

/// The configuration form `response` carries, with the Space's
/// [`FIELD_SPACE_FEDERATED`] field added.
pub(crate) fn with_federated_field(mut response: Iq, federated: bool) -> Iq {
    let field = || {
        Element::builder("field", waddle_xmpp::xep::xep0004::NS_DATA_FORMS)
            .attr(
                minidom::rxml::xml_ncname!("var").to_owned(),
                FIELD_SPACE_FEDERATED,
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "boolean")
            .attr(
                minidom::rxml::xml_ncname!("label").to_owned(),
                "Open to accounts at other servers",
            )
            .append(
                Element::builder("value", waddle_xmpp::xep::xep0004::NS_DATA_FORMS)
                    .append(if federated { "1" } else { "0" })
                    .build(),
            )
            .build()
    };
    if let Iq::Result {
        payload: Some(pubsub),
        ..
    } = &mut response
    {
        for configure in pubsub
            .children_mut()
            .filter(|child| child.name() == "configure")
        {
            for form in configure
                .children_mut()
                .filter(|child| child.is("x", waddle_xmpp::xep::xep0004::NS_DATA_FORMS))
            {
                form.append_child(field());
            }
        }
    }
    response
}

/// The value of [`FIELD_SPACE_FEDERATED`] in the configuration form `iq`
/// submits: `Ok(None)` when the form does not carry the field, and `Err`
/// for a value that is no XEP-0004 boolean.
pub(crate) fn submitted_federated(iq: &Iq) -> Result<Option<bool>, ()> {
    let Iq::Set { payload, .. } = iq else {
        return Ok(None);
    };
    let value = payload
        .children()
        .filter(|child| child.name() == "configure")
        .flat_map(Element::children)
        .filter(|child| child.is("x", waddle_xmpp::xep::xep0004::NS_DATA_FORMS))
        .flat_map(Element::children)
        .find(|field| field.name() == "field" && field.attr("var") == Some(FIELD_SPACE_FEDERATED))
        .map(|field| {
            field
                .children()
                .find(|child| child.name() == "value")
                .map(Element::text)
        });
    match value {
        None => Ok(None),
        Some(Some(value)) => match value.trim() {
            "1" | "true" => Ok(Some(true)),
            "0" | "false" => Ok(Some(false)),
            _ => Err(()),
        },
        Some(None) => Err(()),
    }
}

/// The room a Space item names, when it is a XEP-0402 bookmark of a room
/// of this deployment's MUC service; `None` for any other item.
fn bookmarked_room(state: &WebSocketState, id: &str, payload: Option<&Element>) -> Option<BareJid> {
    if payload.is_some_and(|payload| !payload.is("conference", NS_BOOKMARKS2)) {
        return None;
    }
    let room = id.parse::<BareJid>().ok()?;
    (room.node().is_some() && room.domain().as_str() == state.deps.service_domains.muc)
        .then_some(room)
}

/// Whether a Space item is one of the Space's images (XEP-0503 0.3.0,
/// "Spaces images") that a remote principal who may read the Space is told
/// of: its id names an image, and its payload, where it carries one, is
/// that image's metadata. A retraction carries none.
fn space_image(id: &str, payload: Option<&Element>) -> bool {
    SpaceImage::from_item_id(id).is_some()
        && payload.is_none_or(|payload| validate_space_image_metadata(payload).is_ok())
}

/// Whether `room` is open to accounts at other servers at all, read as the
/// room's own host reads it for every request
/// ([`super::s2s_rooms::room_open_to_other_servers`]): from the room's
/// actor where this replica holds one, and otherwise from the channel
/// catalog. `None` when that cannot be told now; such a room is not told.
async fn room_open(state: &WebSocketState, room: &BareJid) -> Option<bool> {
    super::s2s_rooms::room_open_to_other_servers(state, room).await
}

/// Whether the grants of `room` let the remote principal `account`,
/// deciding on `evidence`, be told of it: always for a room no enrolled
/// host-grant scope manages, which describes itself to any admitted asker,
/// and for a managed room only where its canonical grants admit the
/// principal to view it on that evidence. `None` when that cannot be
/// decided now; such a room is not told.
async fn room_admits(
    state: &WebSocketState,
    room: &BareJid,
    account: &BareJid,
    evidence: &PrincipalEvidence,
) -> Option<bool> {
    match state
        .deps
        .app_state
        .host_grants
        .allows_as(
            RoomBinding::Room(room),
            account,
            evidence,
            HostGrantAction::View,
        )
        .await
    {
        RoomGate::Unmanaged => Some(true),
        RoomGate::Managed(allowed) => Some(allowed),
        RoomGate::Unavailable => None,
    }
}

/// Whether a remote principal `account`, deciding on `evidence`, may be
/// told of `room` as a child of a Space: what the room's own host would
/// tell it ([`room_open`] and [`room_admits`]). `None` when either cannot
/// be told now.
async fn room_visible(
    state: &WebSocketState,
    room: &BareJid,
    account: &BareJid,
    evidence: &PrincipalEvidence,
) -> Option<bool> {
    match room_open(state, room).await {
        Some(true) => room_admits(state, room, account, evidence).await,
        closed_or_unknown => closed_or_unknown,
    }
}

impl Asked<'_> {
    fn refuse(&self, error: StanzaError) -> Vec<Stanza> {
        refuse(self.iq, error)
    }

    fn pubsub_error(&self, error: PubSubError) -> Vec<Stanza> {
        answer(build_pubsub_error(self.iq, error))
    }

    fn storage(&self) -> &Arc<dyn waddle_xmpp::pubsub::PubSubStorage> {
        &self.state.deps.protocol.pubsub_storage
    }

    /// The Space `node`, when this asker is answered about it; `Ok(None)`
    /// for a node answered as one that does not exist, and `Err` when that
    /// cannot be told now.
    ///
    /// The `federated` setting is read first, by the node's name alone, so
    /// a failure to read it answers every name alike; any later failure is
    /// reached only for a name whose setting is on. Nothing else about the
    /// node, the asker's standing in it included, is looked at before that.
    async fn space(&self, node: &str) -> Result<Option<OpenSpace>, Undecided> {
        let enabled_for = FederatedSpaces::new(database(self.state).clone())
            .enabled_for(&self.service, node)
            .await
            .map_err(|_| Undecided)?;
        match enabled_for {
            Some(enabled_for) => self.space_enabled_for(node, enabled_for).await,
            None => Ok(None),
        }
    }

    /// [`Self::space`] for a node whose `federated` setting is on for the
    /// node created at `enabled_for`.
    async fn space_enabled_for(
        &self,
        node: &str,
        enabled_for: i64,
    ) -> Result<Option<OpenSpace>, Undecided> {
        // The Space of an enrolled host-grant scope is served to nobody:
        // no grant of the scope is a Space's, so nobody's standing in it
        // can be decided.
        match self
            .state
            .deps
            .app_state
            .host_grants
            .space_enrolled(node)
            .await
        {
            Ok(false) => {}
            Ok(true) => return Ok(None),
            Err(_) => return Err(Undecided),
        }
        let stored = self
            .storage()
            .get_node(&self.service, node)
            .await
            .map_err(|_| Undecided)?;
        let Some(stored) =
            stored.filter(|stored| stored.created_at.timestamp_millis() == enabled_for)
        else {
            return Ok(None);
        };
        let affiliation = self
            .storage()
            .affiliation(&self.service, node, &self.account)
            .await
            .map_err(|_| Undecided)?;
        let affiliation = honoured(&self.witness, affiliation);
        // A private Space is told only to those it admits; a public one to
        // anybody, a banned account included, which is refused in it.
        let told = match stored.config.access_model {
            AccessModel::Open => true,
            access => reads(access, affiliation),
        };
        Ok(told.then_some(OpenSpace {
            node: stored,
            affiliation,
        }))
    }

    /// The open Space `node`, or the answer that refuses the request.
    async fn open(&self, node: &str) -> Result<OpenSpace, Vec<Stanza>> {
        match self.space(node).await {
            Ok(Some(space)) => Ok(space),
            Ok(None) => Err(self.refuse(no_such_node())),
            Err(Undecided) => Err(self.refuse(internal())),
        }
    }

    /// The Space's name as its metadata records it, or its node name.
    async fn title(&self, node: &str) -> String {
        match self
            .state
            .deps
            .app_state
            .spaces_metadata_store
            .get_by_node(&SpaceNode::from(node))
            .await
        {
            Ok(Some(metadata)) => metadata.name,
            _ => node.to_owned(),
        }
    }

    /// Whether this asker may be told of the Space item `item` as a room of
    /// the Space: a bookmark of a room it may be told of. A Space's images
    /// are no rooms ([`space_image`]); a read of its items serves them
    /// besides. `None` when whether the room is open to other servers, or
    /// its grants, cannot be read now: the item is not told, and a listing
    /// that left it out tells no count.
    async fn child_visible(&self, id: &str, payload: Option<&Element>) -> Option<bool> {
        match bookmarked_room(self.state, id, payload) {
            Some(room) => room_visible(self.state, &room, &self.account, &self.evidence).await,
            None => Some(false),
        }
    }

    // --- service discovery and ping ------------------------------------------

    /// XEP-0030 §3: the service's identity and what it is, or a Space
    /// node's identity with the meta-data form that names it a Space
    /// (XEP-0503, "Fetching the metadata associated to a space"). The
    /// features told are service discovery and the PubSub protocol; any
    /// further one stays unadvertised until its use across servers has
    /// been demonstrated (ADR-0020 gate G21).
    async fn disco_info(&self) -> Vec<Stanza> {
        let Iq::Get { payload, .. } = self.iq else {
            return self.refuse(bad_request());
        };
        let Ok(query) = DiscoInfoQuery::try_from(payload.clone()) else {
            return self.refuse(bad_request());
        };
        let features = vec![
            Feature::new(DISCO_INFO_NS),
            Feature::new(DISCO_ITEMS_NS),
            Feature::new(NS_PUBSUB),
        ];
        let Some(node) = query.node else {
            let info = DiscoInfoResult {
                node: None,
                identities: vec![Identity {
                    category: "pubsub".to_owned(),
                    type_: "service".to_owned(),
                    lang: None,
                    name: None,
                }],
                features,
                extensions: Vec::new(),
            };
            return result(self.iq, Some(Element::from(info)));
        };
        let space = match self.open(&node).await {
            Ok(space) => space,
            Err(refused) => return refused,
        };
        if space.affiliation.is_outcast() {
            return self.refuse(forbidden("You are banned from this Space."));
        }
        result(self.iq, Some(self.node_info(node).await))
    }

    async fn protected_disco_info(&self, principal: &RemotePrincipal) -> Option<SpaceReply> {
        let Iq::Get { payload, .. } = self.iq else {
            return None;
        };
        let node = DiscoInfoQuery::try_from(payload.clone()).ok()?.node?;
        let key = ScopeKey::new(self.service.clone(), node.as_str()).ok()?;
        let grant = self
            .state
            .deps
            .app_state
            .host_grants
            .space_read_remote(&key, principal, self.iq)
            .await?;
        let stanza = result(self.iq, Some(self.node_info(node).await)).pop()?;
        Some(SpaceReply::Protected { stanza, grant })
    }

    async fn node_info(&self, node: String) -> Element {
        let title = self.title(&node).await;
        let form = build_spaces_metadata_form_for_remote(&title);
        let info = DiscoInfoResult {
            node: Some(node),
            identities: vec![Identity {
                category: "pubsub".to_owned(),
                type_: "leaf".to_owned(),
                lang: None,
                name: Some(title),
            }],
            features: vec![Feature::new(DISCO_INFO_NS), Feature::new(NS_PUBSUB)],
            extensions: Vec::new(),
        };
        let mut query = Element::from(info);
        query.append_child(form);
        query
    }

    /// XEP-0503, "Fetching spaces from a spaces service": the Spaces this
    /// asker would be answered about, of the at most [`MAX_LISTED_SPACES`]
    /// Spaces whose `federated` setting is on, sorted by node and paged
    /// with XEP-0059. When more Spaces than that are on, or a Space could
    /// not be decided now, the answer tells no count. A node on the query
    /// asks for the Space's children ([`Self::children`]).
    async fn disco_items(&self) -> Vec<Stanza> {
        let Iq::Get { payload, .. } = self.iq else {
            return self.refuse(bad_request());
        };
        let Ok(query) = DiscoItemsQuery::try_from(payload.clone()) else {
            return self.refuse(bad_request());
        };
        if let Some(node) = query.node {
            return self.children(node, query.rsm.as_ref()).await;
        }
        let Ok(mut federated) = FederatedSpaces::new(database(self.state).clone())
            .list(&self.service, MAX_LISTED_SPACES.saturating_add(1))
            .await
        else {
            return self.refuse(internal());
        };
        // One more than the bound is read to tell whether any was left out.
        let mut complete = federated.len() <= MAX_LISTED_SPACES;
        federated.truncate(MAX_LISTED_SPACES);
        let mut items = Vec::new();
        for (node, enabled_for) in federated {
            let space = match self.space_enabled_for(&node, enabled_for).await {
                Ok(Some(space)) => space,
                Ok(None) => continue,
                // A Space that cannot be decided now is left out of the
                // list, which then tells no count.
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            if space.affiliation.is_outcast() {
                continue;
            }
            items.push(xmpp_parsers::disco::Item {
                jid: Jid::from(self.service.clone()),
                name: Some(self.title(&node).await),
                node: Some(node),
            });
        }
        items.sort_by(|a, b| a.node.cmp(&b.node));
        let (items, rsm) = page_listing(
            items,
            query.rsm.as_ref(),
            MAX_LISTED_SPACES,
            complete,
            |item| item.node.as_deref().unwrap_or_default(),
        );
        result(
            self.iq,
            Some(Element::from(DiscoItemsResult {
                node: None,
                items,
                rsm,
            })),
        )
    }

    /// XEP-0503, "Advertising space membership": the rooms the Space `node`
    /// lists that this asker may be told of ([`Self::child_visible`]), as
    /// the `disco#items` of its node, so that a client verifies that a room
    /// it was told of is part of the Space. Each is listed as XEP-0060 §5.5
    /// lists a published item: the service's address, and the item ID, the
    /// room's JID, as its name. The node is answered as [`Self::open`]
    /// answers it, and an account that may not read the Space is refused as
    /// a read of its items is. Only a room's bookmark is a room of the
    /// Space: an item without a payload is not.
    ///
    /// The Space's [`MAX_RETURNED_ITEMS`] most recently published items are
    /// read, as a read of its items reads them; a room among older items of
    /// a Space that holds more is not listed to accounts at other servers,
    /// and such a listing tells no count, since what it left out is not
    /// known. Nor does a listing that left out a room whose openness to
    /// other servers or grants could not be read now. The rooms are sorted
    /// by item ID and paged with XEP-0059.
    async fn children(&self, node: String, rsm: Option<&SetQuery>) -> Vec<Stanza> {
        let space = match self.open(&node).await {
            Ok(space) => space,
            Err(refused) => return refused,
        };
        if !reads(space.node.config.access_model, space.affiliation) {
            return self.refuse(forbidden("You may not read this Space."));
        }
        // One more than the bound is read to tell whether any was left out;
        // the oldest of them is then dropped.
        let Ok(mut stored) = self
            .storage()
            .get_items(
                &self.service,
                &node,
                Some(MAX_RETURNED_ITEMS.saturating_add(1)),
                &[],
            )
            .await
        else {
            return self.refuse(internal());
        };
        let most = usize::try_from(MAX_RETURNED_ITEMS).unwrap_or(usize::MAX);
        let mut complete = stored.len() <= most;
        if !complete {
            stored.drain(..stored.len() - most);
        }
        let mut items = Vec::new();
        for item in &stored {
            let item = item.to_pubsub_item();
            let Some(id) = item.id else {
                continue;
            };
            if item.payload.is_none() {
                continue;
            }
            match self.child_visible(&id, item.payload.as_ref()).await {
                Some(true) => items.push(xmpp_parsers::disco::Item {
                    jid: Jid::from(self.service.clone()),
                    node: None,
                    name: Some(id),
                }),
                Some(false) => {}
                None => complete = false,
            }
        }
        items.sort_by(|a, b| a.name.cmp(&b.name));
        items.dedup_by(|a, b| a.name == b.name);
        let (items, rsm) = page_listing(items, rsm, most, complete, |item| {
            item.name.as_deref().unwrap_or_default()
        });
        result(
            self.iq,
            Some(Element::from(DiscoItemsResult {
                node: Some(node),
                items,
                rsm,
            })),
        )
    }

    // --- XEP-0060 ---------------------------------------------------------

    async fn pubsub(&self, operation: SpaceOperation) -> Vec<Stanza> {
        let Ok(request) = parse_pubsub_iq(self.iq) else {
            return self.refuse(bad_request());
        };
        // The namespace a request arrived in is the one its operation
        // belongs to: nothing of the owner's is reached through `pubsub`,
        // and the reverse.
        let owner_namespace = self.iq_payload_ns() == Some(NS_PUBSUB_OWNER);
        if owner_namespace != (operation == SpaceOperation::Owner) {
            return self.refuse(bad_request());
        }
        match request {
            PubSubRequest::Items {
                node,
                max_items,
                item_ids,
            } => self.items(&node, max_items, &item_ids).await,
            PubSubRequest::Subscribe { node, jid } => self.subscribe(&node, &jid).await,
            PubSubRequest::Unsubscribe { node, jid, subid } => {
                self.unsubscribe(&node, &jid, subid.as_deref()).await
            }
            // No Space is created for a remote principal: creating one
            // makes its creator the owner.
            PubSubRequest::CreateNode { .. } => self.refuse(forbidden(
                "Spaces are created by accounts of this deployment.",
            )),
            PubSubRequest::Unsupported { feature } => {
                self.pubsub_error(PubSubError::UnsupportedFeature(feature))
            }
            request @ (PubSubRequest::Publish { .. }
            | PubSubRequest::Retract { .. }
            | PubSubRequest::ConfigureNode { .. }
            | PubSubRequest::ConfigureNodeSet { .. }
            | PubSubRequest::DeleteNode { .. }
            | PubSubRequest::PurgeNode { .. }
            | PubSubRequest::AffiliationsGet { .. }
            | PubSubRequest::AffiliationsSet { .. }
            | PubSubRequest::OwnerSubscriptionsGet { .. }
            | PubSubRequest::OwnerSubscriptionsSet { .. }) => self.change(request).await,
            // A request kind this build does not classify is not served.
            _ => self.refuse(unavailable()),
        }
    }

    fn iq_payload_ns(&self) -> Option<&'static str> {
        match self.iq {
            Iq::Get { payload, .. } | Iq::Set { payload, .. } => {
                if payload.is("pubsub", NS_PUBSUB_OWNER) {
                    Some(NS_PUBSUB_OWNER)
                } else if payload.is("pubsub", NS_PUBSUB) {
                    Some(NS_PUBSUB)
                } else {
                    None
                }
            }
            Iq::Result { .. } | Iq::Error { .. } => None,
        }
    }

    /// XEP-0503, "Discovering the content of a space": the bookmarks of the
    /// Space's rooms this asker may be told of, and the Space's avatar and
    /// banner items ("Spaces images", [`space_image`]), read from the store
    /// and nothing else. A session's read also publishes bookmarks the
    /// Space lacks and repairs a room's parent; a remote principal's read
    /// writes nothing.
    async fn items(&self, node: &str, max_items: Option<u32>, item_ids: &[String]) -> Vec<Stanza> {
        if !matches!(self.iq, Iq::Get { .. }) {
            return self.refuse(bad_request());
        }
        let space = match self.open(node).await {
            Ok(space) => space,
            Err(refused) => return refused,
        };
        if !reads(space.node.config.access_model, space.affiliation) {
            return self.refuse(forbidden("You may not read this Space."));
        }
        let most = max_items.map_or(MAX_RETURNED_ITEMS, |asked| asked.min(MAX_RETURNED_ITEMS));
        let Ok(stored) = self
            .storage()
            .get_items(&self.service, node, Some(most), item_ids)
            .await
        else {
            return self.refuse(internal());
        };
        let mut items: Vec<PubSubItem> = Vec::new();
        for item in &stored {
            let item = item.to_pubsub_item();
            let told = match item.id.as_deref() {
                Some(id) if SpaceImage::from_item_id(id).is_some() => {
                    item.payload.is_some() && space_image(id, item.payload.as_ref())
                }
                Some(id) => self.child_visible(id, item.payload.as_ref()).await == Some(true),
                None => false,
            };
            if told {
                // The publisher of an item is an account of this
                // deployment and is not named to a remote asker.
                items.push(PubSubItem {
                    publisher: None,
                    ..item
                });
            }
        }
        answer(build_pubsub_items_result(self.iq, node, &items))
    }

    /// XEP-0503, "Joining a space": a subscription to the Space's node, for
    /// the sender's own account only. The PubSub service keeps one
    /// subscription for an account, under its bare JID, whichever of its
    /// addresses asked; the answer names that bare JID, and notifications
    /// are addressed to it. The subscription is recorded with what the
    /// sender's stream proved, within the bounds on remote subscriptions,
    /// and grants nothing in any room of the Space.
    async fn subscribe(&self, node: &str, jid: &Jid) -> Vec<Stanza> {
        if !matches!(self.iq, Iq::Set { .. }) {
            return self.refuse(bad_request());
        }
        // XEP-0060 §6.1.3.1: the subscribed address must be the sender's.
        // Decided before the node is looked at, for every node alike.
        if jid.to_bare() != self.account {
            return self.pubsub_error(PubSubError::InvalidJid);
        }
        let space = match self.open(node).await {
            Ok(space) => space,
            Err(refused) => return refused,
        };
        if !reads(space.node.config.access_model, space.affiliation) {
            return self.refuse(forbidden("You may not subscribe to this Space."));
        }
        let subscriptions = SpaceSubscriptions::new(database(self.state).clone());
        let PrincipalEvidence::Stream(evidence) = self.evidence else {
            // A request without a stream is no remote principal's.
            return self.refuse(internal());
        };
        let subscriber = Jid::from(self.account.clone());
        let admitted = admitted_now(self.state);
        let recorded = subscriptions
            .record(
                &self.service,
                node,
                space.created_at_ms(),
                SubscriptionRequest {
                    account: &self.account,
                    evidence,
                    snapshot: self.snapshot,
                    now_ms: chrono::Utc::now().timestamp_millis(),
                },
                &admitted,
            )
            .await;
        match recorded.as_ref().map(|recording| recording.outcome) {
            Ok(SubscriptionRecord::Recorded) => {}
            Ok(SubscriptionRecord::Bounds) => {
                telemetry::subscription_limited();
                return self.refuse(busy());
            }
            // The Space was closed to other servers since it was read.
            Ok(SubscriptionRecord::NotFederated) => return self.refuse(no_such_node()),
            Err(_) => return self.refuse(internal()),
        }
        if let Ok(recording) = &recorded {
            self.forget_removed(&recording.removed).await;
        }
        #[cfg(feature = "test-support")]
        subscription_gates::recorded(node, &self.account).await;
        // A ban written since the Space was read above is read again now
        // that the row is committed. A ban is written before its removal of
        // the banned account's row, so either this finds the ban or that
        // removal finds the row. A banned account is answered as its
        // request is when the ban was there first: refused in a public
        // Space, told nothing of a private one.
        let banned = self
            .storage()
            .affiliation(&self.service, node, &self.account)
            .await
            .map(|affiliation| affiliation.is_outcast());
        if !matches!(banned, Ok(false)) {
            // The service holds no subscription for the account yet, so a
            // row left behind is never notified; it is removed when the
            // account unsubscribes or the Space closes, and lapses with
            // its lifetime otherwise.
            if let Err(error) = subscriptions
                .remove(&self.service, node, &self.account)
                .await
            {
                tracing::warn!(
                    %error,
                    "the remote subscription record of a banned account was not removed"
                );
            }
            return match (banned, space.node.config.access_model) {
                (Ok(_), AccessModel::Open) => {
                    self.refuse(forbidden("You may not subscribe to this Space."))
                }
                (Ok(_), _) => self.refuse(no_such_node()),
                (Err(_), _) => self.refuse(internal()),
            };
        }
        let subscribed = match self
            .storage()
            .subscribe(&self.service, node, &subscriber)
            .await
        {
            Ok(subscription) => subscription,
            Err(_) => {
                // Nothing is delivered for a record without the service's
                // own subscription; it is removed all the same.
                let _ = subscriptions
                    .remove(&self.service, node, &self.account)
                    .await;
                return self.refuse(internal());
            }
        };
        // Closing the Space removes the remote subscription records it
        // finds. One written after that ran is found here, by reading the
        // setting again, and removed with this account's row.
        let still_open = FederatedSpaces::new(database(self.state).clone())
            .is_federated(&self.service, node, space.created_at_ms())
            .await;
        if !matches!(still_open, Ok(true)) {
            let _ = self
                .storage()
                .unsubscribe(&self.service, node, &subscriber, Some(&subscribed.subid))
                .await;
            let _ = subscriptions
                .remove(&self.service, node, &self.account)
                .await;
            return match still_open {
                Ok(_) => self.refuse(no_such_node()),
                Err(_) => self.refuse(internal()),
            };
        }
        answer(build_pubsub_subscribe_result(
            self.iq,
            node,
            &subscriber,
            &subscribed.subid,
        ))
    }

    /// Remove the PubSub service's records of the subscriptions the store
    /// removed while it recorded this request's: lapsed ones, and those of
    /// domains no longer admitted. A record that cannot be removed now is
    /// left; without its row nothing is sent for it.
    async fn forget_removed(&self, removed: &[RemovedSubscription]) {
        for gone in removed {
            if self.state.serves(&gone.account) {
                continue;
            }
            let subscriber = Jid::from(gone.account.clone());
            if let Err(error) = self
                .storage()
                .unsubscribe(&gone.service, &gone.node, &subscriber, None)
                .await
            {
                tracing::warn!(
                    %error,
                    "the PubSub record of a removed remote subscription was not removed"
                );
            }
        }
    }

    /// XEP-0060 §6.2: the account's own subscription ends. Leaving only
    /// reduces what the account receives, so whatever the account holds
    /// for the node's name is removed even where the node is not a Space
    /// open to it any more (its membership of a private Space was
    /// withdrawn, the Space was closed to other servers); the answer is
    /// then the one a node that does not exist gets, as for every other
    /// request.
    async fn unsubscribe(&self, node: &str, jid: &Jid, subid: Option<&str>) -> Vec<Stanza> {
        if !matches!(self.iq, Iq::Set { .. }) {
            return self.refuse(bad_request());
        }
        // XEP-0060 §6.2.3.3, decided before the node is looked at.
        if jid.to_bare() != self.account {
            return self.refuse(forbidden("Only your own subscription can be removed."));
        }
        let open = self.open(node).await;
        let subscriber = Jid::from(self.account.clone());
        let subid = subid.map(SubId::from_raw);
        let removed = self
            .storage()
            .unsubscribe(&self.service, node, &subscriber, subid.as_ref())
            .await;
        let forgotten = SpaceSubscriptions::new(database(self.state).clone())
            .remove(&self.service, node, &self.account)
            .await;
        if let Err(refused) = open {
            return refused;
        }
        match (removed, forgotten) {
            (Ok(true), Ok(())) => answer(build_pubsub_success(self.iq)),
            (Ok(false), Ok(())) => self.refuse(not_subscribed()),
            _ => self.refuse(internal()),
        }
    }

    /// A request that changes a Space: publishing and retracting an item,
    /// which places a room in the Space or takes it out, and everything of
    /// the `pubsub#owner` namespace. It needs honoured privilege first
    /// (ADR-0020 D8: a trusted home on a `pkix` stream), whatever the
    /// Space's own policy would allow, and is then decided by the checks a
    /// session's request meets, for the remote account as their subject.
    async fn change(&self, request: PubSubRequest) -> Vec<Stanza> {
        let node = match &request {
            PubSubRequest::Publish { node, .. }
            | PubSubRequest::Retract { node, .. }
            | PubSubRequest::ConfigureNode { node }
            | PubSubRequest::ConfigureNodeSet { node, .. }
            | PubSubRequest::DeleteNode { node }
            | PubSubRequest::PurgeNode { node }
            | PubSubRequest::AffiliationsGet { node }
            | PubSubRequest::AffiliationsSet { node, .. }
            | PubSubRequest::OwnerSubscriptionsGet { node }
            | PubSubRequest::OwnerSubscriptionsSet { node, .. } => node.clone(),
            _ => return self.refuse(unavailable()),
        };
        let space = match self.open(&node).await {
            Ok(space) => space,
            Err(refused) => return refused,
        };
        if space.affiliation.is_outcast() || !self.witness.privilege_honoured() {
            return self.refuse(forbidden(
                "Changing a Space from another server requires a privileged standing in it, a \
                 trusted server and a certificate-authenticated stream.",
            ));
        }
        // An owner's change of subscriptions may name a JID at another
        // server. Nothing is sent to it: a notification leaves only for a
        // subscription the remote account made itself.
        let answers = spaces_change_for_remote(self.state, self.iq, &self.account, request).await;
        if answers.is_empty() {
            return self.refuse(internal());
        }
        answers
            .into_iter()
            .map(|iq| Stanza::Iq(Box::new(iq)))
            .collect()
    }
}

#[async_trait]
impl LocalSpaces for WebSocketLocalSpaces {
    async fn handle(&self, request: SpaceRequest) -> SpaceReply {
        let Some(state) = self.state.upgrade() else {
            return SpaceReply::Ordinary(Vec::new());
        };
        let Stanza::Iq(iq) = &request.stanza else {
            return SpaceReply::Ordinary(Vec::new());
        };
        let iq = iq.as_ref();
        if !matches!(iq, Iq::Get { .. } | Iq::Set { .. }) {
            return SpaceReply::Ordinary(Vec::new());
        }
        let account = request.principal.account().as_bare_jid().clone();
        let evidence = PrincipalEvidence::of(&request.principal);
        // The D1 decision where the effect happens: a domain the snapshot
        // this replica runs does not admit, or a stream below host policy's
        // strength, is refused whatever a Space holds for the account.
        let Some(witness) = principal_witness(&state, &account, &evidence)
            .filter(PrincipalWitness::grants_honoured)
        else {
            telemetry::not_admitted();
            return SpaceReply::Ordinary(refuse(
                iq,
                forbidden("This service does not admit your server."),
            ));
        };
        // The service is its bare domain: no node of it and no Space has
        // an address of its own for a remote principal.
        let Some(service) = iq
            .to()
            .filter(|to| to.node().is_none() && to.resource().is_none())
            .map(Jid::to_bare)
            .filter(|to| to.as_str() == state.deps.service_domains.spaces)
        else {
            return SpaceReply::Ordinary(refuse(iq, unavailable()));
        };
        let asked = Asked {
            state: &state,
            iq,
            service,
            account,
            evidence,
            witness,
            snapshot: request.principal.generation(),
        };
        let answers = match request.operation {
            SpaceOperation::DiscoInfo => {
                if let Some(reply) = asked.protected_disco_info(&request.principal).await {
                    return reply;
                }
                asked.disco_info().await
            }
            SpaceOperation::DiscoItems => asked.disco_items().await,
            SpaceOperation::Ping => match iq {
                Iq::Get { .. } => result(iq, None),
                _ => refuse(iq, bad_request()),
            },
            operation @ (SpaceOperation::PubSub | SpaceOperation::Owner) => {
                asked.pubsub(operation).await
            }
        };
        SpaceReply::Ordinary(answers)
    }

    fn published(&self, publication: SpacePublication) -> bool {
        // The allowance is taken before anything is queued or read, so one
        // Space's burst spends its own allowance and not the queue's places.
        if !self.within_allowance(&publication) {
            telemetry::publication_limited();
            return false;
        }
        match self
            .publications
            .try_send(Queued::Publication(Box::new(publication)))
        {
            Ok(()) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                telemetry::publication_dropped();
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                telemetry::publications_discarded(1);
                false
            }
        }
    }

    async fn publications_settled(&self) {
        let (settled, done) = tokio::sync::oneshot::channel();
        if self
            .publications
            .send(Queued::Settled(settled))
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
}

/// The item a Space's event notification names, and so the room a
/// subscriber must be allowed to learn of to receive it, or the Space's
/// image it names.
fn event_item(event: &Message) -> Option<(String, Option<Element>)> {
    let items = event
        .payloads
        .iter()
        .filter(|payload| payload.is("event", waddle_xmpp::pubsub::NS_PUBSUB_EVENT))
        .flat_map(Element::children)
        .find(|child| child.name() == "items")?;
    let named = items
        .children()
        .find(|child| matches!(child.name(), "item" | "retract"))?;
    let id = named.attr("id")?.to_owned();
    Some((id, named.children().next().cloned()))
}

/// The event notification as a subscriber at another server receives it:
/// the event as built, without the address of the account that published
/// the item.
fn for_remote_subscriber(event: &Message, to: &Jid) -> Message {
    let mut message = event.clone();
    message.to = Some(to.clone());
    for payload in &mut message.payloads {
        if !payload.is("event", waddle_xmpp::pubsub::NS_PUBSUB_EVENT) {
            continue;
        }
        for items in payload.children_mut() {
            for item in items.children_mut() {
                if item.name() == "item" {
                    item.set_attr(
                        minidom::rxml::Namespace::NONE,
                        minidom::rxml::xml_ncname!("publisher").to_owned(),
                        None::<String>,
                    );
                }
            }
        }
    }
    message
}

/// Hand `event`, an event notification of the Space `node` of `service`
/// just stored, to the host's notification task for the Space's
/// subscribers at other servers. It only queues: the publisher's request
/// waits for none of what is decided about them. Nothing is queued while
/// the replica runs no S2S snapshot.
pub(crate) fn queue_remote_notification(
    state: &WebSocketState,
    service: &BareJid,
    node: &PubSubNode,
    event: &Message,
) {
    if state.deps.app_state.s2s_federation.is_none() {
        return;
    }
    // A route that serves no Spaces to remote principals takes nothing,
    // and counts nothing.
    let _ = state.deps.remote_route.space_published(SpacePublication {
        service: service.clone(),
        node: node.node_name.clone(),
        node_created_at_ms: node.created_at.timestamp_millis(),
        event: event.clone(),
    });
}

/// Send the event notification of `publication` to the Space's subscribers
/// at other servers (XEP-0060 §7.1), on the host's notification task.
///
/// A notification leaves only where all of this holds when it is handed
/// over:
///
/// - the Space still exists as the node the publication was made to, it is
///   open to other servers and the replica serves its Spaces to them;
/// - the subscriber made the subscription itself over an inbound stream,
///   within the last thirty days, and the service's own record still holds
///   it as deliverable, read for this publication;
/// - the subscriber's domain is admitted now (D1) and its standing in the
///   Space, honoured for the evidence its subscription recorded (D8),
///   still lets it read the Space;
/// - the room the event names is one the subscriber may be told of, or the
///   event names one of the Space's images ([`space_image`]), which any
///   subscriber that reads the Space is told of.
///
/// One publication, queued within the Space's allowance of notified
/// publications, sends at most one stanza to each of at most 256 remote
/// accounts; what does not fit a domain pair's queue is dropped, as a
/// headline may be. Nothing is sent to an address at a domain this
/// deployment serves.
async fn notify_remote_subscribers(state: &WebSocketState, publication: &SpacePublication) {
    // Without an S2S snapshot nothing leaves for another server, and
    // nothing is read to find that out.
    if state.deps.app_state.s2s_federation.is_none() {
        return;
    }
    let SpacePublication {
        service,
        node: node_name,
        node_created_at_ms: created_at_ms,
        event,
    } = publication;
    let created_at_ms = *created_at_ms;
    let db = database(state).clone();
    let federated = FederatedSpaces::new(db.clone())
        .is_federated(service, node_name, created_at_ms)
        .await;
    if !matches!(federated, Ok(true)) {
        return;
    }
    // The node as it is now: the publication is decided after its
    // publisher was answered, and a Space deleted since tells nothing.
    let node = match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(service, node_name)
        .await
    {
        Ok(Some(node)) if node.created_at.timestamp_millis() == created_at_ms => node,
        Ok(_) => return,
        Err(_) => {
            tracing::warn!("event notifications withheld: the Space could not be read");
            return;
        }
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let Ok(subscriptions) = SpaceSubscriptions::new(db)
        .of_node(service, node_name, created_at_ms, now_ms)
        .await
    else {
        tracing::warn!("event notifications withheld: the remote subscriptions could not be read");
        return;
    };
    if subscriptions.is_empty() {
        return;
    }
    let Some((item_id, item_payload)) = event_item(event) else {
        return;
    };
    // The room the event names; an image of the Space names none.
    let room = if space_image(&item_id, item_payload.as_ref()) {
        None
    } else {
        let Some(room) = bookmarked_room(state, &item_id, item_payload.as_ref()) else {
            return;
        };
        // Whether the room is open to other servers is the same for every
        // subscriber: a room that is not is told to none of them.
        if room_open(state, &room).await != Some(true) {
            return;
        }
        Some(room)
    };
    // The service's own record, read for this publication just before the
    // first hand-over: a subscription removed, or an account banned, since
    // the remote subscription was recorded receives nothing.
    let Ok(deliverable) = state
        .deps
        .protocol
        .pubsub_storage
        .list_deliverable_subscribers(service, node_name)
        .await
    else {
        tracing::warn!("event notifications withheld: the subscriptions could not be read");
        return;
    };
    let deliverable: Vec<BareJid> = deliverable
        .into_iter()
        .map(|subscription| subscription.subscriber.to_bare())
        .collect();
    for subscription in subscriptions {
        let account = &subscription.account;
        let subscriber = Jid::from(account.clone());
        let evidence = PrincipalEvidence::Stream(subscription.evidence);
        let sent = !state.serves(account)
            && deliverable.contains(account)
            && subscriber_reads(state, service, &node, account, &evidence).await
            && match &room {
                Some(room) => room_admits(state, room, account, &evidence).await == Some(true),
                None => true,
            }
            && state.deps.remote_route.space_event(
                service,
                &subscriber,
                Stanza::Message(for_remote_subscriber(event, &subscriber)),
            );
        if !sent {
            telemetry::event_withheld();
        }
    }
}

/// Whether the remote `account` may read the Space `node` now, on the
/// evidence its subscription recorded: its domain is admitted, and its
/// affiliation, as honoured for that evidence, lets it read a Space of the
/// node's access model.
async fn subscriber_reads(
    state: &WebSocketState,
    service: &BareJid,
    node: &PubSubNode,
    account: &BareJid,
    evidence: &PrincipalEvidence,
) -> bool {
    let Some(witness) =
        principal_witness(state, account, evidence).filter(PrincipalWitness::grants_honoured)
    else {
        return false;
    };
    let Ok(stored) = state
        .deps
        .protocol
        .pubsub_storage
        .affiliation(service, &node.node_name, account)
        .await
    else {
        return false;
    };
    reads(node.config.access_model, honoured(&witness, stored))
}

/// A point of a remote account's subscription that a suite holds, to write
/// what a concurrent request would write there: just after the account's
/// subscription record committed, before the ban is read again.
#[cfg(feature = "test-support")]
pub(crate) mod subscription_gates {
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};

    use jid::BareJid;
    use tokio::sync::Notify;

    type Key = (String, BareJid);

    static GATES: LazyLock<Mutex<HashMap<Key, Arc<SubscriptionGate>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// A held point: the next subscription of the account to the node that
    /// reaches it waits there until the suite releases it.
    #[derive(Default)]
    pub struct SubscriptionGate {
        reached: Notify,
        released: Notify,
    }

    impl SubscriptionGate {
        /// Wait until a subscription has reached the point.
        pub async fn wait_until_reached(&self) {
            self.reached.notified().await;
        }

        /// Let the subscription that reached the point go on.
        pub fn release(&self) {
            self.released.notify_one();
        }
    }

    /// Hold the next subscription of `account` to the Space `node` once its
    /// record has committed.
    pub fn hold_after_record(node: &str, account: BareJid) -> Arc<SubscriptionGate> {
        let gate = Arc::new(SubscriptionGate::default());
        GATES
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .insert((node.to_owned(), account), Arc::clone(&gate));
        gate
    }

    pub(super) async fn recorded(node: &str, account: &BareJid) {
        let gate = GATES
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .remove(&(node.to_owned(), account.clone()));
        if let Some(gate) = gate {
            gate.reached.notify_one();
            gate.released.notified().await;
        }
    }
}
