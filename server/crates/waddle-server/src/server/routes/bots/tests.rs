use axum::body::Body;
use axum::http::{HeaderMap, Method, Request};
use http_body_util::BodyExt;
use kameo::actor::Spawn;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::auth::bots::BOT_TOKEN_PREFIX;
use crate::auth::Session;
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::routes::session_auth::SESSION_ID_HEADER;
use crate::server::routes::websocket::tests::create_test_websocket_state;

const TRUSTED_ORIGIN: &str = "http://localhost:3000";

struct Fixture {
    socket: Arc<WebSocketState>,
    state: BotsState,
}

async fn fixture() -> Fixture {
    fixture_with_limit(10).await
}

async fn fixture_with_limit(limit: u32) -> Fixture {
    let socket = create_test_websocket_state().await;
    let state = BotsState::new(
        Arc::clone(&socket.deps.auth_state),
        Arc::clone(&socket.deps.app_state),
        Arc::clone(&socket),
        &socket.deps.service_domains.bots,
        limit,
    );
    Fixture { socket, state }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("JSON body: {}", self.body))
    }

    fn error(&self) -> String {
        self.json()["error"]
            .as_str()
            .expect("error code")
            .to_string()
    }
}

impl Fixture {
    fn auth(&self) -> &AuthState {
        &self.state.auth
    }

    fn bots_domain(&self) -> &str {
        self.state.bots_domain.as_str()
    }

    fn permissions(&self) -> &ActorRef<PermissionActor> {
        &self.state.auth.permission_actor
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        let response = router(Arc::new(self.state.clone()))
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8(body.to_vec()).expect("UTF-8 body"),
        }
    }

    /// A signed-in human; returns the session id and bare JID.
    async fn user(&self, localpart: &str) -> (String, BareJid) {
        let jid = format!("{localpart}@{}", self.auth().xmpp_domain);
        let session = Session::new(&jid, localpart, localpart);
        self.auth()
            .session_manager
            .create_session(&session)
            .await
            .expect("create session");
        (session.id, jid.parse().expect("user JID"))
    }

    /// A signed-in server member, allowed to create bots.
    async fn member(&self, localpart: &str) -> (String, BareJid) {
        let (session, jid) = self.user(localpart).await;
        self.permissions()
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                    Relation::new("member"),
                    Subject::user(jid.to_string()),
                ),
            })
            .await
            .expect("server member tuple");
        (session, jid)
    }

    async fn create(&self, session: &str, name: &str) -> Reply {
        self.send(post_json("/api/bots", session, json!({ "name": name })))
            .await
    }

    async fn create_ok(&self, session: &str, name: &str) -> String {
        let reply = self.create(session, name).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        reply.json()["jid"].as_str().expect("bot jid").to_string()
    }

    async fn mint(&self, session: &str, bot: &str) -> Reply {
        self.send(post_json(
            &format!("{}/tokens", bot_path(bot)),
            session,
            json!({ "label": "laptop" }),
        ))
        .await
    }

    async fn list(&self, session: &str) -> Reply {
        self.send(with_header(Request::get("/api/bots"), session))
            .await
    }

    async fn check(&self, subject: &str, permission: Permission, object: Object) -> bool {
        self.permissions()
            .ask(CheckPermission {
                subject: Subject::user(subject),
                permission,
                object,
            })
            .await
            .expect("permission check")
            .allowed
    }

    fn bot(&self, raw: &str) -> BotJid {
        BotJid::parse(raw, &self.state.bots_domain).expect("bot JID")
    }
}

fn bot_path(bot: &str) -> String {
    format!("/api/bots/{}", urlencoding::encode(bot))
}

fn with_header(builder: axum::http::request::Builder, session: &str) -> Request<Body> {
    builder
        .header(SESSION_ID_HEADER, session)
        .body(Body::empty())
        .expect("request")
}

fn post_json(uri: &str, session: &str, body: Value) -> Request<Body> {
    Request::post(uri)
        .header(SESSION_ID_HEADER, session)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn cookie_request(method: Method, uri: &str, session: &str, origin: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header(header::COOKIE, format!("waddle_session={session}"));
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if method == Method::POST {
        builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "name": "standup" }).to_string()))
            .expect("request")
    } else {
        builder.body(Body::empty()).expect("request")
    }
}

// ---------------------------------------------------------------------------
// Session and CSRF
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_session_is_401() {
    let f = fixture().await;
    let reply = f
        .send(
            Request::get("/api/bots")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.error(), "invalid_session");
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn header_post_without_origin_creates_201() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let reply = f.create(&session, "standup").await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn cookie_post_without_origin_is_403_untrusted_origin() {
    let f = fixture().await;
    let (session, owner) = f.member("alice").await;
    let reply = f
        .send(cookie_request(Method::POST, "/api/bots", &session, None))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.error(), "untrusted_origin");
    assert!(f
        .auth()
        .bot_store
        .list_for_owner(&owner)
        .await
        .expect("list")
        .is_empty());
}

#[tokio::test]
async fn cookie_post_foreign_origin_is_403() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    for origin in ["https://attacker.example", "null"] {
        let reply = f
            .send(cookie_request(
                Method::POST,
                "/api/bots",
                &session,
                Some(origin),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin}");
        assert_eq!(reply.error(), "untrusted_origin");
    }
}

#[tokio::test]
async fn cookie_post_trusted_origin_creates_201() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let reply = f
        .send(cookie_request(
            Method::POST,
            "/api/bots",
            &session,
            Some(TRUSTED_ORIGIN),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn cookie_get_without_origin_is_200() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let reply = f
        .send(cookie_request(Method::GET, "/api/bots", &session, None))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let body = reply.json();
    assert_eq!(body["bots"], json!([]));
    assert_eq!(body["limit"], 10);
    assert_eq!(body["token_limit"], MAX_ACTIVE_TOKENS_PER_BOT);
}

#[tokio::test]
async fn error_body_never_echoes_session_id() {
    let f = fixture().await;
    let presented = "presented-session-id-0123456789";
    let header = f
        .send(with_header(Request::get("/api/bots"), presented))
        .await;
    let cookie = f
        .send(cookie_request(Method::GET, "/api/bots", presented, None))
        .await;
    let mutation = f
        .send(cookie_request(Method::POST, "/api/bots", presented, None))
        .await;
    for reply in [header, cookie, mutation] {
        assert!(reply.status.is_client_error());
        assert!(
            !reply.body.contains(presented),
            "error body echoed the session id: {}",
            reply.body
        );
    }
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_returns_plus_jid_on_bots_domain() {
    let f = fixture().await;
    let (session, owner) = f.member("alice").await;
    let reply = f
        .send(post_json(
            "/api/bots",
            &session,
            json!({ "name": "standup", "display_name": "  Standup Bot  " }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
    let body = reply.json();
    assert_eq!(
        body["jid"],
        format!("alice+standup@{}", f.bots_domain()).as_str()
    );
    assert_eq!(body["name"], "standup");
    assert_eq!(body["display_name"], "Standup Bot");
    assert_eq!(body["owner_jid"], owner.as_str());
    assert_eq!(body["disabled_at"], Value::Null);
    assert_eq!(body["disabled_reason"], Value::Null);
    assert_eq!(body["tokens"], json!([]));
}

#[tokio::test]
async fn create_rejects_bad_names_and_bodies() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    for name in ["", "Standup", "a+b", "-standup", "x".repeat(33).as_str()] {
        let reply = f.create(&session, name).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{name:?}");
        assert_eq!(reply.error(), "invalid_name");
    }
    let unknown_field = f
        .send(post_json(
            "/api/bots",
            &session,
            json!({ "name": "standup", "owner": "bob@example.com" }),
        ))
        .await;
    assert_eq!(unknown_field.status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown_field.error(), "invalid_request");
    let not_json = f
        .send(
            Request::post("/api/bots")
                .header(SESSION_ID_HEADER, &session)
                .body(Body::from("name=standup"))
                .expect("request"),
        )
        .await;
    assert_eq!(not_json.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn create_writes_owner_tuple() {
    let f = fixture().await;
    let (session, owner) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;
    let object = Object::new(ObjectType::Bot, bot.as_str());
    assert!(
        f.check(owner.as_str(), Permission::Manage, object.clone())
            .await
    );
    let (_, bob) = f.member("bob").await;
    assert!(!f.check(bob.as_str(), Permission::Manage, object).await);
}

#[tokio::test]
async fn create_without_server_membership_is_403() {
    let f = fixture().await;
    let (session, owner) = f.user("alice").await;
    let reply = f.create(&session, "standup").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.error(), "forbidden");
    assert!(f
        .auth()
        .bot_store
        .list_for_owner(&owner)
        .await
        .expect("list")
        .is_empty());
}

#[tokio::test]
async fn create_over_limit_is_409_bot_limit_reached() {
    let f = fixture_with_limit(1).await;
    let (session, _) = f.member("alice").await;
    f.create_ok(&session, "first").await;
    let reply = f.create(&session, "second").await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.error(), "bot_limit_reached");

    let disabled = fixture_with_limit(0).await;
    let (session, _) = disabled.member("alice").await;
    assert_eq!(
        disabled.create(&session, "standup").await.error(),
        "bot_limit_reached"
    );
}

#[tokio::test]
async fn create_duplicate_is_409_bot_exists() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    f.create_ok(&session, "standup").await;
    let reply = f.create(&session, "standup").await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.error(), "bot_exists");
}

#[tokio::test]
async fn create_rolls_back_row_when_tuple_write_fails() {
    let f = fixture().await;
    let (_, owner) = f.member("alice").await;
    let bot = BotJid::new(
        &owner,
        &BotName::try_from("standup").expect("name"),
        &f.state.bots_domain,
    )
    .expect("bot JID");
    let store = &f.auth().bot_store;
    assert_eq!(
        store.create(&bot, &owner, None, 10).await.expect("insert"),
        CreateBotOutcome::Created
    );

    // A permission backend that is gone fails the tuple write.
    let database = Arc::new(f.socket.deps.app_state.db_pool.global().clone());
    let broken = PermissionActor::spawn(PermissionActor::new_for_tests(database));
    broken.kill();
    broken.wait_for_shutdown().await;

    let error = write_owner_tuple_or_rollback(&broken, store, &bot, &owner)
        .await
        .expect_err("the tuple write fails");
    assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.code(), "bots_unavailable");
    assert_eq!(store.get(&bot).await.expect("get"), None);
}

#[tokio::test]
async fn create_retry_repairs_missing_owner_tuple() {
    let f = fixture().await;
    let (session, owner) = f.member("alice").await;
    // A create that died between the insert and the tuple write.
    let bot = BotJid::new(
        &owner,
        &BotName::try_from("standup").expect("name"),
        &f.state.bots_domain,
    )
    .expect("bot JID");
    assert_eq!(
        f.auth()
            .bot_store
            .create(&bot, &owner, None, 10)
            .await
            .expect("insert"),
        CreateBotOutcome::Created
    );
    let object = Object::new(ObjectType::Bot, bot.to_string());
    assert!(
        !f.check(owner.as_str(), Permission::Manage, object.clone())
            .await
    );

    let reply = f.create(&session, "standup").await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.error(), "bot_exists");
    assert!(f.check(owner.as_str(), Permission::Manage, object).await);

    let deleted = f
        .send(with_header(
            Request::delete(bot_path(bot.as_bare().as_str())),
            &session,
        ))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    assert_eq!(f.auth().bot_store.get(&bot).await.expect("get"), None);
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mint_returns_secret_once_and_list_never_contains_it() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;

    let minted = f.mint(&session, &bot).await;
    assert_eq!(minted.status, StatusCode::CREATED, "{}", minted.body);
    let body = minted.json();
    let secret = body["secret"].as_str().expect("secret");
    assert!(secret.starts_with(BOT_TOKEN_PREFIX));
    assert_eq!(secret.len(), 47);
    assert_eq!(body["token"]["label"], "laptop");
    assert_eq!(body["token"]["revoked_at"], Value::Null);
    assert!(body["token"].get("secret").is_none());
    assert!(body["token"].get("token_hash").is_none());

    let list = f.list(&session).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(!list.body.contains(BOT_TOKEN_PREFIX), "{}", list.body);
    assert!(!list.body.contains(secret));
    let tokens = &list.json()["bots"][0]["tokens"];
    assert_eq!(tokens.as_array().map(Vec::len), Some(1));
    assert_eq!(tokens[0]["id"], body["token"]["id"]);
}

#[tokio::test]
async fn mint_response_has_no_store() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;
    let minted = f.mint(&session, &bot).await;
    assert_eq!(minted.status, StatusCode::CREATED);
    assert_eq!(minted.headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn mint_rejects_bad_label_and_ttl() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;
    let uri = format!("{}/tokens", bot_path(&bot));
    for body in [
        json!({ "label": "" }),
        json!({ "label": "x".repeat(65) }),
        json!({ "label": "ci", "expires_in_days": 0 }),
        json!({ "label": "ci", "expires_in_days": 366 }),
        json!({ "label": "ci", "expires_in_days": 100000 }),
    ] {
        let reply = f.send(post_json(&uri, &session, body.clone())).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(reply.error(), "invalid_token_request", "{body}");
    }
}

#[tokio::test]
async fn mint_over_cap_is_409_bot_token_limit_reached() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;
    for _ in 0..MAX_ACTIVE_TOKENS_PER_BOT {
        assert_eq!(f.mint(&session, &bot).await.status, StatusCode::CREATED);
    }
    let reply = f.mint(&session, &bot).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.error(), "bot_token_limit_reached");
}

#[tokio::test]
async fn mint_for_disabled_bot_is_409_bot_disabled() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;
    f.socket
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(crate::db::actor::DbExecute {
            sql: "UPDATE users SET disabled_at = ?, disabled_reason = 'owner_not_member' \
                  WHERE jid = ?"
                .to_string(),
            params: vec![
                crate::db::Value::from("2026-01-01T00:00:00.000000Z"),
                crate::db::Value::from(bot.as_str()),
            ],
        })
        .await
        .expect("disable bot");
    let reply = f.mint(&session, &bot).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.error(), "bot_disabled");
    let listed = f.list(&session).await.json();
    assert_eq!(listed["bots"][0]["disabled_reason"], "owner_not_member");
}

#[tokio::test]
async fn revoke_then_validate_is_revoked() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;
    let minted = f.mint(&session, &bot).await.json();
    let secret = minted["secret"].as_str().expect("secret");
    let id = minted["token"]["id"].as_str().expect("token id");
    f.auth()
        .bot_tokens
        .validate(secret)
        .await
        .expect("fresh token validates");

    let uri = format!("{}/tokens/{id}", bot_path(&bot));
    let revoked = f.send(with_header(Request::delete(&uri), &session)).await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.body);
    assert!(matches!(
        f.auth().bot_tokens.validate(secret).await,
        Err(BotTokenError::Revoked)
    ));
    // Revoking again is still a success.
    let again = f.send(with_header(Request::delete(&uri), &session)).await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
    assert_ne!(
        f.list(&session).await.json()["bots"][0]["tokens"][0]["revoked_at"],
        Value::Null
    );
}

#[tokio::test]
async fn revoke_other_users_token_is_404() {
    let f = fixture().await;
    let (alice, _) = f.member("alice").await;
    let (bob, _) = f.member("bob").await;
    let alice_bot = f.create_ok(&alice, "standup").await;
    let bob_bot = f.create_ok(&bob, "standup").await;
    let minted = f.mint(&alice, &alice_bot).await.json();
    let secret = minted["secret"].as_str().expect("secret");
    let id = minted["token"]["id"].as_str().expect("token id");

    // Bob may not reach Alice's bot, nor Alice's token through his own bot.
    for uri in [
        format!("{}/tokens/{id}", bot_path(&alice_bot)),
        format!("{}/tokens/{id}", bot_path(&bob_bot)),
        format!("{}/tokens/not-a-token-id", bot_path(&bob_bot)),
    ] {
        let reply = f.send(with_header(Request::delete(&uri), &bob)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(reply.error(), "not_found");
    }
    f.auth()
        .bot_tokens
        .validate(secret)
        .await
        .expect("Alice's token still validates");
}

// ---------------------------------------------------------------------------
// Bot-scoped paths and delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_owner_delete_is_404() {
    let f = fixture().await;
    let (alice, _) = f.member("alice").await;
    let (bob, _) = f.member("bob").await;
    let bot = f.create_ok(&alice, "standup").await;

    let reply = f
        .send(with_header(Request::delete(bot_path(&bot)), &bob))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.error(), "not_found");
    // Same answer as for a bot that does not exist.
    let missing = f
        .send(with_header(
            Request::delete(bot_path(&format!("bob+ghost@{}", f.bots_domain()))),
            &bob,
        ))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body, reply.body);
    assert!(f
        .auth()
        .bot_store
        .get(&f.bot(&bot))
        .await
        .expect("get")
        .is_some());
}

#[tokio::test]
async fn delete_purges_grants_and_tokens() {
    let f = fixture().await;
    let (session, owner) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;
    f.mint(&session, &bot).await;
    let channel = Object::new(ObjectType::Channel, "general");
    f.permissions()
        .ask(WriteTuple {
            tuple: Tuple::new(
                channel.clone(),
                Relation::new("member"),
                Subject::user(bot.as_str()),
            ),
        })
        .await
        .expect("channel grant");
    // Warm the permission cache, so a stale cached allow would show.
    assert!(f.check(&bot, Permission::Read, channel.clone()).await);

    let reply = f
        .send(with_header(Request::delete(bot_path(&bot)), &session))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");

    assert!(!f.check(&bot, Permission::Read, channel).await);
    let remaining = f
        .permissions()
        .ask(crate::permissions::ListSubjectTuples {
            subject: Subject::user(bot.as_str()),
        })
        .await
        .expect("list tuples");
    assert!(remaining.is_empty(), "{remaining:?}");
    assert!(
        !f.check(
            owner.as_str(),
            Permission::Manage,
            Object::new(ObjectType::Bot, bot.as_str()),
        )
        .await
    );
    assert!(f
        .auth()
        .bot_tokens
        .list_for_bot(&f.bot(&bot))
        .await
        .expect("tokens")
        .is_empty());
    assert_eq!(
        f.auth().bot_store.get(&f.bot(&bot)).await.expect("get"),
        None
    );
    assert_eq!(f.list(&session).await.json()["bots"], json!([]));

    // Recreating the same name yields the same JID with no old grants.
    assert_eq!(f.create_ok(&session, "standup").await, bot);
    assert!(
        !f.check(
            &bot,
            Permission::Read,
            Object::new(ObjectType::Channel, "general")
        )
        .await
    );
}

#[tokio::test]
async fn path_jid_percent_encoded_plus_decodes() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    let bot = f.create_ok(&session, "standup").await;
    let uri = format!("/api/bots/alice%2Bstandup%40{}/tokens", f.bots_domain());
    let reply = f
        .send(post_json(&uri, &session, json!({ "label": "ci" })))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(
        f.list(&session).await.json()["bots"][0]["jid"],
        bot.as_str()
    );
}

#[tokio::test]
async fn invalid_jid_wrong_domain_is_400() {
    let f = fixture().await;
    let (session, _) = f.member("alice").await;
    f.create_ok(&session, "standup").await;
    for raw in [
        "alice+standup@example.com".to_string(),
        "alice+standup@bots.attacker.example".to_string(),
        format!("alice@{}", f.bots_domain()),
        f.bots_domain().to_string(),
        "not a jid".to_string(),
    ] {
        let reply = f
            .send(with_header(Request::delete(bot_path(&raw)), &session))
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{raw}");
        assert_eq!(reply.error(), "invalid_jid", "{raw}");
    }
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bots_api_counter_records_op_and_outcome() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let f = fixture().await;
    let (session, _) = f.member("alice").await;

    let bot = f.create_ok(&session, "standup").await;
    assert_eq!(f.list(&session).await.status, StatusCode::OK);
    assert_eq!(
        f.create(&session, "standup").await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(f.mint(&session, &bot).await.status, StatusCode::CREATED);
    let unauthenticated = f
        .send(
            Request::get("/api/bots")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);
    let deleted = f
        .send(with_header(Request::delete(bot_path(&bot)), &session))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);

    let count = |operation: &str, outcome: &str| {
        metrics.counter_sum(
            "waddle.bots.api.requests",
            &[("operation", operation), ("outcome", outcome)],
        )
    };
    assert_eq!(count("create", "success"), Some(1));
    assert_eq!(count("create", "failure"), Some(1));
    assert_eq!(count("list", "success"), Some(1));
    assert_eq!(count("list", "failure"), Some(1));
    assert_eq!(count("mint_token", "success"), Some(1));
    assert_eq!(count("delete", "success"), Some(1));
    assert_eq!(
        metrics.metric_unit("waddle.bots.api.requests"),
        Some("{request}".to_string())
    );
}
