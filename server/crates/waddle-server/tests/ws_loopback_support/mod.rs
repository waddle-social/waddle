//! An in-process server for suites that drive the production `/ws` router
//! (RFC 7395) against a test state they also set up directly: accounts,
//! catalog rows, permission relationships and PubSub nodes.
//!
//! Clients authenticate with SASL SCRAM-SHA-256 against native accounts and
//! bind a resource, as a deployed client does. Stanzas are built and parsed
//! as typed values.

use std::sync::Arc;

use jid::BareJid;
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::db::actor::DbExecute;
use waddle_server::test_support::{router, seed_local_account, WebSocketState};
use waddle_ws_test_support::WsXmppClient;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::DefinedCondition;

/// The test state pins its account domain (`test_state.rs`).
pub const DOMAIN: &str = "example.com";
/// The test state's MUC service.
pub const MUC: &str = "muc.example.com";
/// The test state's Spaces service.
pub const SPACES: &str = "spaces.example.com";
/// The test state's extensions service.
pub const EXTENSIONS: &str = "extensions.example.com";
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
pub const NS_MUC: &str = "http://jabber.org/protocol/muc";
pub const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
pub const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
pub const NS_MAM: &str = "urn:xmpp:mam:2";
pub const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const PASSWORD: &str = "loopback-account-password";

/// A test state served over a loopback TCP listener.
pub struct Loopback {
    pub state: Arc<WebSocketState>,
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Loopback {
    pub async fn serve(state: Arc<WebSocketState>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("listener address");
        let app = router(Arc::clone(&state));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            state,
            url: format!("ws://{addr}/ws"),
            server,
        }
    }

    /// Create the native account `localpart@example.com`.
    pub async fn account(&self, localpart: &str) -> BareJid {
        seed_local_account(&self.state, localpart).await;
        NativeUserStore::new(self.state.deps.app_state.db_pool.global_actor().clone())
            .register(RegisterRequest {
                username: localpart.to_owned(),
                domain: DOMAIN.to_owned(),
                password: PASSWORD.to_owned(),
                email: None,
            })
            .await
            .expect("register native account");
        format!("{localpart}@{DOMAIN}")
            .parse()
            .expect("account JID")
    }

    /// An authenticated client of `localpart` with `resource` bound.
    pub async fn client(&self, localpart: &str, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(&self.url, DOMAIN, localpart, PASSWORD, resource)
            .await
            .expect("authenticated client")
    }

    /// Insert or replace a channel catalog row, as the admin commands and
    /// the topology bootstrap write it.
    pub async fn channel(&self, id: &str, members_only: bool) {
        let now = chrono::Utc::now().to_rfc3339();
        self.state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbExecute {
                sql: "INSERT INTO channels (id, name, description, channel_type, position, \
                      is_default, pin_permission, members_only, public_room, created_at, \
                      updated_at) VALUES (?, ?, ?, 'text', 0, ?, 'anyone', ?, ?, ?, ?) \
                      ON CONFLICT(id) DO UPDATE SET members_only = excluded.members_only"
                    .to_owned(),
                params: vec![
                    id.into(),
                    id.into(),
                    None::<String>.into(),
                    false.into(),
                    members_only.into(),
                    true.into(),
                    now.clone().into(),
                    now.into(),
                ],
            })
            .await
            .expect("channel catalog row");
    }
}

/// The room JID of the channel `channel_id`.
pub fn room(channel_id: &str) -> BareJid {
    format!("{channel_id}@{MUC}").parse().expect("room JID")
}

/// Send `iq` and wait for its reply.
pub async fn round_trip(client: &mut WsXmppClient, iq: Iq) -> Iq {
    waddle_ws_test_support::iq::round_trip(client, iq).await
}

/// Whether an IQ reply is a result.
pub fn is_result(reply: &Iq) -> bool {
    matches!(reply, Iq::Result { .. })
}

/// Whether an IQ reply is an error with `condition`.
pub fn is_error(reply: &Iq, condition: DefinedCondition) -> bool {
    matches!(reply, Iq::Error { error, .. } if error.defined_condition == condition)
}

/// A `set` IQ to `to` carrying `payload`.
pub fn set(id: &str, to: &str, payload: Element) -> Iq {
    Iq::Set {
        from: None,
        to: Some(to.parse().expect("IQ destination")),
        id: id.to_owned(),
        payload,
    }
}

/// A `get` IQ to `to` carrying `payload`.
pub fn get(id: &str, to: &str, payload: Element) -> Iq {
    Iq::Get {
        from: None,
        to: Some(to.parse().expect("IQ destination")),
        id: id.to_owned(),
        payload,
    }
}

/// A XEP-0060 publish of one item to `node`.
pub fn publish(node: &str, item_id: &str, payload: Element) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("publish", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .append(
                    Element::builder("item", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                        .append(payload)
                        .build(),
                )
                .build(),
        )
        .build()
}

/// A XEP-0060 subscription of `jid` to `node`.
pub fn subscribe(node: &str, jid: &str) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("subscribe", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
                .build(),
        )
        .build()
}

/// A XEP-0060 items request for `node`.
pub fn items(node: &str) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("items", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .build(),
        )
        .build()
}
