//! XEP-0045 private event creation and room-scoped XEP-0060 planning through
//! the production RFC 7395 router and the native Android/Apple FFI verbs.
//! Server members can create private events while ordinary channels remain owner-only.
#![cfg(feature = "test-support")]
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use waddle_server::permissions::{
    DeleteTuple, Object, ObjectType, Relation, Subject, Tuple, WriteTuple,
};
use waddle_server::test_support::{create_test_session, router, websocket_state};
use waddle_xmpp_client_ffi::{
    WaddleClient, WaddleClientEvent, WaddleConfig, WaddleEventListener, WaddlePlanningItem,
    WaddlePlanningKind, WaddlePlanningStatus,
};
struct Listener(mpsc::UnboundedSender<WaddleClientEvent>);
impl WaddleEventListener for Listener {
    fn on_event(&self, event: WaddleClientEvent) {
        let _ = self.0.send(event);
    }
}
fn metadata() -> WaddlePlanningItem {
    WaddlePlanningItem {
        id: "event".into(),
        kind: WaddlePlanningKind::Event,
        author: None,
        title: "Rust Nation".into(),
        description: Some("Waddle In-Person".into()),
        url: None,
        category: None,
        location: Some("London England".into()),
        timezone: Some("Europe/London".into()),
        starts_at: Some(1802768400000),
        ends_at: Some(1803056400000),
        assignee: None,
        completed: false,
        pinned: false,
        amount_minor: None,
        currency: None,
        participants: Vec::new(),
        options: Vec::new(),
        selections: Vec::new(),
        subject: None,
        attendance: None,
        status: Some(WaddlePlanningStatus::Confirmed),
    }
}
async fn connect(
    url: &str,
    jid: &str,
    token: &str,
    resource: &str,
) -> (
    Arc<WaddleClient>,
    mpsc::UnboundedReceiver<WaddleClientEvent>,
) {
    let (events, mut receiver) = mpsc::unbounded_channel();
    let client = WaddleClient::new(
        WaddleConfig {
            server_url: url.into(),
            jid: jid.into(),
            access_token: token.into(),
            resource: resource.into(),
            resumable: false,
        },
        Box::new(Listener(events)),
    );
    client.connect().await;
    timeout(Duration::from_secs(15), async {
        loop {
            match receiver.recv().await.expect("client event stream") {
                WaddleClientEvent::Connected => break,
                WaddleClientEvent::Error { description } => {
                    panic!("connect {resource}: {description}")
                }
                WaddleClientEvent::AuthenticationFailed { condition } => {
                    panic!("auth {resource}: {condition:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("native session ready");
    (client, receiver)
}

async fn assert_hidden_from_disco(url: &str, jid: &str, token: &str, room: &str) {
    use waddle_xmpp_client::discovery::DiscoveryExt;
    use waddle_xmpp_client::{
        AccessToken, ClientConfig, ClientError, ClientEvent, ClientResource, ConnectionConfig,
        LifecycleEvent, OAuthBearerConfig, WebSocketConfig, XmppClient,
    };
    let account: jid::BareJid = jid.parse().expect("disco account");
    let domain = account.domain().as_str().parse().expect("disco domain");
    let auth = OAuthBearerConfig::try_new(
        account,
        ClientResource::try_new("outsider-disco").expect("resource"),
        AccessToken::new(token),
    )
    .expect("auth config");
    let config = ClientConfig::try_new(
        ConnectionConfig::new(domain),
        WebSocketConfig::try_new(url.parse().expect("url")).expect("transport"),
        auth,
    )
    .expect("client config");
    let client = XmppClient::try_new(config).expect("native disco client");
    let handle = client
        .driver()
        .expect("driver")
        .connect()
        .await
        .expect("connect disco");
    let mut events = handle.events();
    timeout(Duration::from_secs(15), async {
        loop {
            if matches!(
                events.recv().await.expect("disco lifecycle"),
                ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_))
            ) {
                break;
            }
        }
    })
    .await
    .expect("disco client ready");
    let result = handle.discover_info(room, None).await;
    assert!(
        matches!(result, Err(ClientError::StanzaError(ref error)) if error.condition == "item-not-found"),
        "private event title is undiscoverable: {result:?}"
    );
    handle.disconnect().await.expect("disconnect disco");
}

#[tokio::test]
async fn native_owner_and_member_can_create_private_trip_and_read_back_metadata() {
    let state = websocket_state().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("listener address");
    let app = router(Arc::clone(&state));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve websocket");
    });
    for (username, relation) in [("tripowner", "owner"), ("tripmember", "member")] {
        let session = create_test_session(&state, username).await;
        state
            .deps
            .app_state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Server, "deployment"),
                    Relation::new(relation),
                    Subject::user(&session.user_jid),
                ),
            })
            .await
            .expect("server membership");
        let url = format!("ws://{address}/ws");
        let (client, _) = connect(&url, &session.user_jid, &session.id, username).await;
        let room = client
            .create_event_room("Rust Nation".into(), true)
            .await
            .unwrap_or_else(|error| panic!("native {relation} create_event_room: {error:?}"));
        let bookmarks = client
            .fetch_user_bookmarks()
            .await
            .expect("native bookmarks");
        assert!(
            bookmarks.iter().any(|bookmark| bookmark.jid == room),
            "new trip is discoverable"
        );
        let saved = client
            .event_planning_publish(room.clone(), metadata())
            .await
            .unwrap_or_else(|error| panic!("native {relation} publish metadata: {error:?}"));
        assert_eq!(saved.title, "Rust Nation");
        assert_eq!(saved.author.as_deref(), Some(session.user_jid.as_str()));
        let reloaded = client
            .event_planning_items(room.clone())
            .await
            .expect("reload workspace");
        assert!(reloaded
            .iter()
            .any(|item| item.id == "event" && item.title == saved.title));
        let mut link = metadata();
        link.id = "hotel".into();
        link.kind = WaddlePlanningKind::Link;
        link.title = "London hotel".into();
        link.url = Some("https://example.test/london-hotel".into());
        link.starts_at = None;
        link.ends_at = None;
        link.status = None;
        client
            .event_planning_publish(room.clone(), link)
            .await
            .expect("save trip bookmark");
        client.disconnect().await;
        let (fresh, _) = connect(
            &url,
            &session.user_jid,
            &session.id,
            &format!("{username}-reload"),
        )
        .await;
        let persisted = fresh
            .event_planning_items(room.clone())
            .await
            .expect("fresh client reload");
        assert_eq!(persisted.len(), 2);
        let mut trip = persisted
            .into_iter()
            .find(|item| item.id == "event")
            .expect("saved event");
        assert_eq!(trip.starts_at, metadata().starts_at);
        assert_eq!(trip.ends_at, metadata().ends_at);
        assert_eq!(trip.timezone.as_deref(), Some("Europe/London"));
        assert_eq!(trip.description.as_deref(), Some("Waddle In-Person"));
        assert_eq!(trip.location.as_deref(), Some("London England"));
        assert!(matches!(trip.status, Some(WaddlePlanningStatus::Confirmed)));

        trip.description = Some("Updated February travel plan".into());
        fresh
            .event_planning_publish(room.clone(), trip)
            .await
            .expect("edit saved event");
        let config = fresh
            .fetch_room_config(room.clone())
            .await
            .expect("private room config");
        assert_eq!(config.members_only, Some(true));
        assert_eq!(config.public_room, Some(false));
        for patch in [
            waddle_xmpp_client_ffi::WaddleRoomConfigPatch {
                members_only: Some(false),
                ..Default::default()
            },
            waddle_xmpp_client_ffi::WaddleRoomConfigPatch {
                public_room: Some(true),
                ..Default::default()
            },
        ] {
            assert!(
                fresh.submit_room_config(room.clone(), patch).await.is_err(),
                "private workspace cannot be downgraded"
            );
        }
        let outsider_session = create_test_session(&state, &format!("{username}-outsider")).await;
        state
            .deps
            .app_state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Server, "deployment"),
                    Relation::new("member"),
                    Subject::user(&outsider_session.user_jid),
                ),
            })
            .await
            .expect("outsider is a server member");
        let (outsider, mut outsider_events) = connect(
            &url,
            &outsider_session.user_jid,
            &outsider_session.id,
            "outsider",
        )
        .await;
        assert!(
            outsider.event_planning_items(room.clone()).await.is_err(),
            "outsider cannot read trip"
        );
        assert!(
            outsider.fetch_room_config(room.clone()).await.is_err(),
            "outsider cannot read config"
        );
        assert!(
            outsider
                .event_planning_publish(room.clone(), metadata())
                .await
                .is_err(),
            "outsider cannot mutate trip"
        );
        assert!(
            outsider
                .create_room(
                    room.split('@').next().expect("room localpart").into(),
                    "takeover".into(),
                    Default::default()
                )
                .await
                .is_err(),
            "event permission cannot take over an existing workspace"
        );
        assert_hidden_from_disco(
            &url,
            &outsider_session.user_jid,
            &outsider_session.id,
            &room,
        )
        .await;
        outsider.join_room(room.clone(), "uninvited".into()).await;
        timeout(Duration::from_secs(15), async {
            loop {
                if let WaddleClientEvent::Presence { presence } =
                    outsider_events.recv().await.expect("outsider event stream")
                {
                    if presence.error_condition.is_some()
                        && presence.from.as_deref() == Some(format!("{room}/uninvited").as_str())
                    {
                        break;
                    }
                }
            }
        })
        .await
        .expect("private room rejects outsider join");
        state
            .deps
            .app_state
            .permission_actor
            .ask(DeleteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Server, "deployment"),
                    Relation::new("member"),
                    Subject::user(&outsider_session.user_jid),
                ),
            })
            .await
            .expect("revoke outsider server membership");
        assert!(
            outsider
                .create_event_room("Unauthorized trip".into(), true)
                .await
                .is_err(),
            "nonmember cannot create event workspace"
        );
        outsider.disconnect().await;
        if relation == "member" {
            assert!(
                fresh
                    .create_room(
                        format!("ordinary-{}", uuid::Uuid::new_v4()),
                        "tripmember".into(),
                        Default::default()
                    )
                    .await
                    .is_err(),
                "event permission cannot create ordinary channels"
            );
        }
        fresh
            .destroy_room(room, None)
            .await
            .expect("remove test room");
        fresh.disconnect().await;
        client.disconnect().await;
    }
    server.abort();
    let _ = server.await;
}
