//! Shared fixture of the revocation suites: the host-grant fixture's scope
//! served by one or more replicas of one deployment.
//!
//! Each replica is a WebSocket state on the fixture's PostgreSQL database,
//! wired as a PostgreSQL deployment is (`websocket_state_with_host_grant_fence`)
//! and served over its own loopback socket. A replica can also be started
//! in another process, from the database's URL and the deployment's lineage. A replica has its own room
//! registry and room actors, its own enrolled set and its own part of the
//! revocation fence: a lease, the generations it installed and the sweep of
//! the rooms it hosts. The replicas share only the database, which is what
//! the fence is built on. Accounts are created once and are valid on every
//! replica.
//!
//! The fixture's database login owns the schema, so the replicas attach
//! without the runtime-privilege check and without the restore fence, as in
//! the other suites of the canonical store.

use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use uuid::Uuid;
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::config::LineageConfig;
use waddle_server::db::actor::DbExecute;
use waddle_server::db::lineage::DeploymentUuid;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, DatabasePool, PoolConfig};
use waddle_server::ingress::IngressAuthority;
use waddle_server::permissions::host_grant::{HostGrantAttachment, HostGrantStore, ReplicaFence};
use waddle_server::test_support::{
    router, seed_local_account, websocket_state_with_host_grant_fence, WebSocketState,
};
use waddle_ws_test_support::WsXmppClient;
use xmpp_parsers::minidom::Element;

use crate::host_grant_support::{served, GrantFixture};

pub const NS_MUC: &str = "http://jabber.org/protocol/muc";
pub const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
pub const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
pub const NS_MAM: &str = "urn:xmpp:mam:2";
pub const NS_PING: &str = "urn:xmpp:ping";

/// The test state pins its account domain.
pub const DOMAIN: &str = "example.com";
const PASSWORD: &str = "revocation-account-password";

/// How long a case waits for something the revocation sweep sends: the
/// sweep starts within a tick of the fence and retries every second.
pub const SWEEP_BUDGET: Duration = Duration::from_secs(8);

/// One replica of a deployment: a WebSocket state on the deployment's
/// database, served over a loopback socket.
pub struct Replica {
    pub state: Arc<WebSocketState>,
    /// The replica's `/ws` endpoint.
    pub url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Replica {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Replica {
    /// Start a replica on the migrated database at `database_url`, whose
    /// lineage is `deployment`'s. Room-manifest acceptance and every other
    /// surface decide with one attachment, as on a server.
    pub async fn start(database_url: &str, deployment: Uuid) -> Self {
        let config = DatabaseConfig::new(DatabaseDriver::Postgres, database_url);
        let database = Database::from_config("revocation-replica", &config)
            .await
            .expect("replica database");
        let store = HostGrantStore::open(database.clone()).expect("replica store");
        let authority = Arc::new(
            IngressAuthority::try_new(
                Default::default(),
                database,
                LineageConfig {
                    deployment_uuid: Some(DeploymentUuid(deployment)),
                    action: None,
                },
                #[cfg(feature = "clustering")]
                None,
            )
            .await
            .expect("replica ingress authority"),
        );
        assert!(authority.attach_host_grants(
            HostGrantAttachment::unfenced_for_test(&store, served())
                .await
                .expect("attachment")
        ));
        let pool = Arc::new(
            DatabasePool::try_new(config, PoolConfig)
                .await
                .expect("replica database pool"),
        );
        // The fenced state binds its host-grant gate to the authority's
        // attachment gate: offering that gate again binds nothing, and the
        // surfaces decide with the attachment acceptance holds.
        let state = websocket_state_with_host_grant_fence(pool, Arc::clone(&authority)).await;
        let host_grants = &state.deps.app_state.host_grants;
        assert!(!host_grants.bind_attachment(authority.host_grant_gate()));
        assert!(host_grants.attachment().is_some());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let app = router(Arc::clone(&state));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            state,
            url: format!("ws://{address}/ws"),
            server,
        }
    }

    /// Create the native account `localpart@example.com`. Accounts live in
    /// the database, so every replica of the deployment serves them.
    pub async fn account(&self, localpart: &str) {
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
    }

    /// Insert the channel catalog row of a room, as the admin commands and
    /// the topology bootstrap write it.
    pub async fn channel(&self, id: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        self.state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbExecute {
                sql: "INSERT INTO channels (id, name, description, channel_type, position, \
                      is_default, pin_permission, members_only, public_room, created_at, \
                      updated_at) VALUES (?, ?, ?, 'text', 0, ?, 'anyone', ?, ?, ?, ?)"
                    .to_owned(),
                params: vec![
                    id.into(),
                    id.into(),
                    None::<String>.into(),
                    false.into(),
                    false.into(),
                    true.into(),
                    now.clone().into(),
                    now.into(),
                ],
            })
            .await
            .expect("channel catalog row");
    }

    /// An authenticated client of `localpart` on this replica, with
    /// `resource` bound.
    pub async fn client(&self, localpart: &str, resource: &str) -> WsXmppClient {
        connect(&self.url, localpart, resource).await
    }

    /// The replica's part of the revocation fence.
    pub fn fence(&self) -> ReplicaFence {
        self.state
            .deps
            .app_state
            .host_grants
            .fence()
            .expect("a PostgreSQL replica has a fence")
            .clone()
    }
}

/// An authenticated client of `localpart` on the replica at `url`, with
/// `resource` bound.
pub async fn connect(url: &str, localpart: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(url, DOMAIN, localpart, PASSWORD, resource)
        .await
        .expect("authenticated client")
}

/// The fixture's scope served by `replicas.len()` replicas.
pub struct Deployment {
    pub grants: GrantFixture,
    pub replicas: Vec<Replica>,
}

impl Deployment {
    /// The fixture's active scope served by `count` replicas, with the
    /// accounts `owner`, `romeo`, `alice` and `carol`.
    pub async fn start(label: &str, count: usize) -> Option<Self> {
        let grants = GrantFixture::active(label).await?;
        let mut replicas = Vec::new();
        for index in 0..count {
            let replica = Replica::start(
                grants.ingress.db.database_url(),
                grants.ingress.deployment_uuid(),
            )
            .await;
            if index == 0 {
                for account in ["owner", "romeo", "alice", "carol"] {
                    replica.account(account).await;
                }
                // The catalog row says the room is open; the room's binding
                // makes it members-only whatever the row says.
                replica.channel(&grants.scope.channel_id).await;
            }
            replicas.push(replica);
        }
        Some(Self { grants, replicas })
    }

    pub fn room(&self) -> &BareJid {
        &self.grants.scope.room
    }

    pub async fn close(self) {
        drop(self.replicas);
        self.grants.close().await;
    }
}

pub fn presence_join(room: &BareJid, nick: &str) -> String {
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

/// Join `room` as `nick` and return the first presence the room sends from
/// that occupant address: the self-presence or the join error.
pub async fn join(client: &mut WsXmppClient, room: &BareJid, nick: &str) -> Element {
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

/// The defined condition of a presence, message or IQ error, if it is one.
pub fn error_condition(stanza: &Element) -> Option<String> {
    if stanza.attr("type") != Some("error") {
        return None;
    }
    stanza
        .children()
        .find(|child| child.name() == "error")
        .and_then(|error| error.children().next())
        .map(|condition| condition.name().to_owned())
}

pub fn admin_item(affiliation: &str, jid: &str) -> Element {
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

/// An affiliation change with the reason its administrator gives.
pub fn admin_item_with_reason(affiliation: &str, jid: &str, reason: &str) -> Element {
    Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    affiliation,
                )
                .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
                .append(
                    Element::builder("reason", NS_MUC_ADMIN)
                        .append(reason)
                        .build(),
                )
                .build(),
        )
        .build()
}

pub fn role_item(role: &str, nick: &str) -> Element {
    Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(minidom::rxml::xml_ncname!("role").to_owned(), role)
                .attr(minidom::rxml::xml_ncname!("nick").to_owned(), nick)
                .build(),
        )
        .build()
}

pub fn mam_query(id: &str) -> Element {
    Element::builder("query", NS_MAM)
        .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), id)
        .build()
}

/// The XEP-0045 status codes and the item of a room presence.
pub struct RoomPresence {
    pub unavailable: bool,
    pub statuses: Vec<String>,
    pub affiliation: Option<String>,
    pub role: Option<String>,
    /// Whether the item names who acted.
    pub actor: bool,
    /// The reason the item carries.
    pub reason: Option<String>,
}

pub fn room_presence(presence: &Element) -> RoomPresence {
    let user = presence
        .children()
        .find(|child| child.name() == "x" && child.ns() == NS_MUC_USER);
    let item = user.and_then(|user| user.children().find(|child| child.name() == "item"));
    RoomPresence {
        unavailable: presence.attr("type") == Some("unavailable"),
        statuses: user
            .map(|user| {
                user.children()
                    .filter(|child| child.name() == "status")
                    .filter_map(|status| status.attr("code").map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        affiliation: item.and_then(|item| item.attr("affiliation").map(str::to_owned)),
        role: item.and_then(|item| item.attr("role").map(str::to_owned)),
        actor: item.is_some_and(|item| item.children().any(|child| child.name() == "actor")),
        reason: item.and_then(|item| {
            item.children()
                .find(|child| child.name() == "reason")
                .map(Element::text)
        }),
    }
}

/// The next presence `client` receives from the occupant address
/// `room/nick` within `budget`, or `None`.
pub async fn presence_from(
    client: &mut WsXmppClient,
    room: &BareJid,
    nick: &str,
    budget: Duration,
) -> Option<Element> {
    let occupant = format!("{room}/{nick}");
    client
        .recv_matching_within(budget, |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence" && element.attr("from") == Some(occupant.as_str())
            })
        })
        .await
        .ok()
        .and_then(|frame| frame.parse().ok())
}

/// Whether `client` receives a groupchat with `body` from `room` within
/// `budget`.
pub async fn receives_groupchat(
    client: &mut WsXmppClient,
    room: &BareJid,
    body: &str,
    budget: Duration,
) -> bool {
    let prefix = format!("{room}/");
    client
        .recv_matching_within(budget, |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "message"
                    && element.attr("type") == Some("groupchat")
                    && element
                        .attr("from")
                        .is_some_and(|from| from.starts_with(&prefix))
                    && element
                        .children()
                        .any(|child| child.name() == "body" && child.text() == body)
            })
        })
        .await
        .is_ok()
}

pub fn groupchat(room: &BareJid, id: &str, body: &str) -> String {
    String::from(
        &Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room.to_string(),
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .append(
                Element::builder("body", "jabber:client")
                    .append(body)
                    .build(),
            )
            .build(),
    )
}
