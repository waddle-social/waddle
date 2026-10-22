use std::sync::Arc;

use chrono::{Duration, Utc};
use jid::{BareJid, DomainPart};
use kameo::actor::{ActorRef, Spawn};

use super::*;
use crate::auth::token_hash::TokenHasher;
use crate::auth::{AuthError, SessionManager};
use crate::db::actor::{CreateBotOutcome, DbActor, DbExecute, DbQuery, DbQueryOne};
use crate::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner, Value};
use crate::permissions::{
    CheckPermission, DeleteTuple, Object, ObjectType, Permission, PermissionActor, Relation,
    ReplaceExclusiveRelation, Subject, SubjectType, Tuple, WriteTuple,
};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;

const DOMAIN: &str = "waddle.social";
const SESSION_KEY: &[u8] = b"test-session-key";

struct Fixture {
    actor: ActorRef<DbActor>,
    permissions: ActorRef<PermissionActor>,
    store: BotStore,
    tokens: BotTokenStore,
}

async fn fixture(name: &str) -> Fixture {
    let db = Database::in_memory(name).await.expect("in-memory database");
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("migrations");
    fixture_on(db).await
}

async fn fixture_on(db: Database) -> Fixture {
    let actor = DbActor::spawn(DbActor::new(db.clone()));
    let permissions = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(db.clone())));
    let store = BotStore::new(actor.clone());
    let tokens = BotTokenStore::new(
        actor.clone(),
        permissions.clone(),
        DEPLOYMENT_SERVER_ID.to_string(),
        TokenHasher::new(Some(SESSION_KEY)),
    );
    Fixture {
        actor,
        permissions,
        store,
        tokens,
    }
}

fn bots_domain() -> DomainPart {
    DomainPart::new(&format!("bots.{DOMAIN}"))
        .expect("bots domain")
        .into_owned()
}

fn jid(value: &str) -> BareJid {
    value.parse().expect("bare jid")
}

fn name(value: &str) -> BotName {
    BotName::try_from(value).expect("bot name")
}

fn bot_of(owner: &BareJid, bot: &str) -> BotJid {
    BotJid::new(owner, &name(bot), &bots_domain()).expect("bot jid")
}

fn label(value: &str) -> BotTokenLabel {
    BotTokenLabel::try_from(value).expect("token label")
}

async fn execute(f: &Fixture, sql: &str, params: Vec<Value>) -> u64 {
    f.actor
        .ask(DbExecute {
            sql: sql.to_string(),
            params,
        })
        .await
        .expect("execute")
}

async fn query(f: &Fixture, sql: &str, params: Vec<Value>) -> Vec<Vec<Value>> {
    f.actor
        .ask(DbQuery {
            sql: sql.to_string(),
            params,
        })
        .await
        .expect("query")
}

async fn count(f: &Fixture, sql: &str, params: Vec<Value>) -> i64 {
    let row = f
        .actor
        .ask(DbQueryOne {
            sql: sql.to_string(),
            params,
        })
        .await
        .expect("count query")
        .expect("count row");
    match row.first() {
        Some(Value::Integer(value)) => *value,
        other => panic!("expected an integer count, got {other:?}"),
    }
}

/// Inserts a human `users` row for `localpart@waddle.social`.
async fn human(f: &Fixture, localpart: &str) -> BareJid {
    let owner = jid(&format!("{localpart}@{DOMAIN}"));
    let now = Utc::now().to_rfc3339();
    execute(
        f,
        "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        vec![
            Value::from(owner.as_str()),
            Value::from(localpart),
            Value::from(localpart),
            Value::from(localpart),
            Value::from(now.as_str()),
            Value::from(now.as_str()),
        ],
    )
    .await;
    owner
}

fn server_member(user: &BareJid) -> Tuple {
    Tuple::new(
        Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        Relation::new("member"),
        Subject::user(user.to_string()),
    )
}

async fn write(f: &Fixture, tuple: Tuple) {
    f.permissions
        .ask(WriteTuple { tuple })
        .await
        .expect("write tuple");
}

/// A human owner who is a server member.
async fn member(f: &Fixture, localpart: &str) -> BareJid {
    let owner = human(f, localpart).await;
    write(f, server_member(&owner)).await;
    owner
}

async fn create(f: &Fixture, owner: &BareJid, bot: &str) -> BotJid {
    let bot = bot_of(owner, bot);
    assert_eq!(
        f.store.create(&bot, owner, None, 10).await.expect("create"),
        CreateBotOutcome::Created
    );
    bot
}

async fn mint(f: &Fixture, bot: &BotJid) -> MintedBotToken {
    f.tokens
        .mint(bot, label("ci"), BotTokenTtl::default())
        .await
        .expect("mint")
}

/// Inserts a token row directly, for states `mint` never produces.
async fn insert_token(
    f: &Fixture,
    bot: &BotJid,
    secret: &str,
    expires_at: chrono::DateTime<Utc>,
    revoked_at: Option<chrono::DateTime<Utc>>,
) -> BotTokenId {
    let id = BotTokenId::new();
    execute(
        f,
        "INSERT INTO bot_tokens (id, bot_jid, token_hash, label, created_at, last_used_at, \
         expires_at, revoked_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            Value::from(id.to_string()),
            Value::from(bot.as_bare().as_str()),
            Value::from(TokenHasher::new(Some(SESSION_KEY)).bot_token(secret)),
            Value::from("direct"),
            Value::from(storage_timestamp(Utc::now() - Duration::days(30))),
            Value::NullText,
            Value::from(storage_timestamp(expires_at)),
            Value::from(revoked_at.map(storage_timestamp)),
        ],
    )
    .await;
    id
}

fn fake_secret(fill: char) -> String {
    format!("{BOT_TOKEN_PREFIX}{}", fill.to_string().repeat(43))
}

async fn last_used_at(f: &Fixture, id: BotTokenId) -> Option<String> {
    let rows = query(
        f,
        "SELECT last_used_at FROM bot_tokens WHERE id = ?",
        vec![Value::from(id.to_string())],
    )
    .await;
    match rows.first().and_then(|row| row.first()) {
        Some(Value::Text(value)) => Some(value.clone()),
        Some(value) if value.is_null() => None,
        other => panic!("unexpected last_used_at {other:?}"),
    }
}

#[test]
fn bot_name_validation_table() {
    for rejected in [
        "",
        "-a",
        "a-",
        "_a",
        "a_",
        "A",
        "a+b",
        "a.b",
        "a b",
        "ä",
        &"a".repeat(33),
    ] {
        assert_eq!(
            BotName::try_from(rejected),
            Err(BotIdError::InvalidName),
            "{rejected:?}"
        );
    }
    for accepted in ["standup", "a", "7", "a_b-c", "ci-2", &"a".repeat(32)] {
        assert_eq!(
            BotName::try_from(accepted).map(|name| name.as_str().to_owned()),
            Ok(accepted.to_owned()),
            "{accepted:?}"
        );
    }
}

#[test]
fn bot_jid_shape() {
    let owner = jid("rawkode@waddle.social");
    let bot = bot_of(&owner, "standup");
    assert_eq!(bot.to_string(), "rawkode+standup@bots.waddle.social");
    assert_eq!(bot.localpart(), "rawkode+standup");
    assert_eq!(bot.name(), name("standup"));

    let parsed =
        BotJid::parse("rawkode+standup@bots.waddle.social", &bots_domain()).expect("parse bot jid");
    assert_eq!(parsed, bot);
    for invalid in [
        "rawkode+standup@waddle.social",
        "rawkode@bots.waddle.social",
        "+standup@bots.waddle.social",
        "rawkode+@bots.waddle.social",
        "a+b+c@bots.waddle.social",
        "rawkode+Standup!@bots.waddle.social",
        "bots.waddle.social",
    ] {
        assert_eq!(
            BotJid::parse(invalid, &bots_domain()),
            Err(BotIdError::InvalidJid),
            "{invalid:?}"
        );
    }
}

#[test]
fn bot_jid_rejects_plus_owner() {
    assert_eq!(
        BotJid::new(
            &jid("alice+work@waddle.social"),
            &name("standup"),
            &bots_domain()
        ),
        Err(BotIdError::OwnerIneligible)
    );
    assert_eq!(
        BotJid::new(&jid("waddle.social"), &name("standup"), &bots_domain()),
        Err(BotIdError::OwnerIneligible)
    );
}

#[tokio::test]
async fn create_inserts_kind_bot_with_owner() {
    let f = fixture("bots-create-inserts").await;
    let owner = human(&f, "rawkode").await;
    let bot = bot_of(&owner, "standup");
    assert_eq!(
        f.store
            .create(&bot, &owner, Some("Standup Bot".to_string()), 10)
            .await
            .expect("create"),
        CreateBotOutcome::Created
    );

    let rows = query(
        &f,
        "SELECT kind, owner_jid, username, xmpp_localpart, localpart_key, \
         disabled_at IS NULL \
         FROM users WHERE jid = ?",
        vec![Value::from(bot.as_bare().as_str())],
    )
    .await;
    assert_eq!(
        rows,
        vec![vec![
            Value::Text("bot".into()),
            Value::Text(owner.to_string()),
            Value::Text("rawkode+standup".into()),
            Value::Text("rawkode+standup".into()),
            Value::Text("rawkode+standup".into()),
            Value::Integer(1),
        ]]
    );
    // SQLite cannot add the table CHECK, so the store upholds
    // kind = 'bot' <=> owner_jid IS NOT NULL.
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM users \
             WHERE (kind = 'bot' AND owner_jid IS NULL) \
                OR (kind <> 'bot' AND owner_jid IS NOT NULL)",
            vec![],
        )
        .await,
        0
    );

    let record = f.store.get(&bot).await.expect("get").expect("bot exists");
    assert_eq!(record.jid, bot);
    assert_eq!(record.owner, owner);
    assert_eq!(record.name, name("standup"));
    assert_eq!(record.display_name.as_deref(), Some("Standup Bot"));
    assert_eq!(record.disabled, None);
    assert!(f.store.exists_enabled(bot.as_bare()).await.expect("exists"));
    assert!(f
        .store
        .get(&bot_of(&owner, "other"))
        .await
        .expect("get missing")
        .is_none());
}

#[tokio::test]
async fn create_respects_limit() {
    let f = fixture("bots-create-limit").await;
    let owner = human(&f, "rawkode").await;
    for bot in ["one", "two"] {
        assert_eq!(
            f.store
                .create(&bot_of(&owner, bot), &owner, None, 2)
                .await
                .expect("create"),
            CreateBotOutcome::Created
        );
    }
    assert_eq!(
        f.store
            .create(&bot_of(&owner, "three"), &owner, None, 2)
            .await
            .expect("create over limit"),
        CreateBotOutcome::LimitReached
    );
    // Disabled bots still count, and a limit of 0 disables creation.
    execute(
        &f,
        "UPDATE users SET disabled_at = ?, disabled_reason = 'owner_not_member' WHERE jid = ?",
        vec![
            Value::from(storage_timestamp(Utc::now())),
            Value::from(bot_of(&owner, "one").as_bare().as_str()),
        ],
    )
    .await;
    assert_eq!(
        f.store
            .create(&bot_of(&owner, "three"), &owner, None, 2)
            .await
            .expect("create with a disabled bot"),
        CreateBotOutcome::LimitReached
    );
    let other = human(&f, "alice").await;
    assert_eq!(
        f.store
            .create(&bot_of(&other, "one"), &other, None, 0)
            .await
            .expect("create with limit 0"),
        CreateBotOutcome::LimitReached
    );
}

#[tokio::test]
async fn create_duplicate_is_already_exists() {
    let f = fixture("bots-create-duplicate").await;
    let owner = human(&f, "rawkode").await;
    // A legacy human account already holds the bot's account name.
    human(&f, "rawkode+standup").await;
    assert_eq!(
        f.store
            .create(&bot_of(&owner, "standup"), &owner, None, 10)
            .await
            .expect("create over a human name"),
        CreateBotOutcome::AlreadyExists { same_owner: false }
    );

    // A bot row with the same JID but another recorded owner is not the
    // caller's bot.
    let bot = bot_of(&owner, "ci");
    let now = storage_timestamp(Utc::now());
    execute(
        &f,
        "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, kind, owner_jid, \
         created_at, updated_at) VALUES (?, ?, ?, ?, 'bot', ?, ?, ?)",
        vec![
            Value::from(bot.as_bare().as_str()),
            Value::from(bot.localpart()),
            Value::from(bot.localpart()),
            Value::from(bot.localpart()),
            Value::from("mallory@waddle.social"),
            Value::from(now.as_str()),
            Value::from(now.as_str()),
        ],
    )
    .await;
    assert_eq!(
        f.store
            .create(&bot, &owner, None, 10)
            .await
            .expect("create over a foreign row"),
        CreateBotOutcome::AlreadyExists { same_owner: false }
    );
}

#[tokio::test]
async fn create_duplicate_same_owner_reports_same_owner() {
    let f = fixture("bots-create-duplicate-same-owner").await;
    let owner = human(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    assert_eq!(
        f.store
            .create(&bot, &owner, None, 10)
            .await
            .expect("recreate"),
        CreateBotOutcome::AlreadyExists { same_owner: true }
    );
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM users WHERE jid = ?",
            vec![Value::from(bot.as_bare().as_str())],
        )
        .await,
        1
    );
}

#[tokio::test]
async fn create_collides_with_native_localpart() {
    let f = fixture("bots-create-native-collision").await;
    let owner = human(&f, "rawkode").await;
    let bot = bot_of(&owner, "standup");
    // A native account predating the '+' ban, keyed on the primary domain.
    execute(
        &f,
        "INSERT INTO native_users (username, domain, password_hash, salt, stored_key, \
         server_key, jid_key) VALUES (?, ?, 'hash', 'salt', '', '', ?)",
        vec![
            Value::from("rawkode+standup"),
            Value::from(DOMAIN),
            Value::from(format!("rawkode+standup@{DOMAIN}")),
        ],
    )
    .await;
    assert_eq!(
        f.store
            .create(&bot, &owner, None, 10)
            .await
            .expect("create"),
        CreateBotOutcome::AlreadyExists { same_owner: false }
    );
    assert!(f.store.get(&bot).await.expect("get").is_none());
}

#[tokio::test]
async fn create_owner_missing() {
    let f = fixture("bots-create-owner-missing").await;
    let ghost = jid("ghost@waddle.social");
    assert_eq!(
        f.store
            .create(&bot_of(&ghost, "standup"), &ghost, None, 10)
            .await
            .expect("create"),
        CreateBotOutcome::OwnerMissing
    );

    // A bot cannot own bots: only human rows qualify as owners.
    let owner = human(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let nested = bot_of(&jid(&format!("rawkode-standup@{DOMAIN}")), "x");
    assert_eq!(
        f.store
            .create(&nested, bot.as_bare(), None, 10)
            .await
            .expect("create under a bot"),
        CreateBotOutcome::OwnerMissing
    );
}

#[tokio::test]
async fn create_concurrent_respects_limit() {
    // A file database with one actor per create, so the creates race on
    // separate pooled connections instead of queueing on one actor.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open_local("bots-create-concurrent", dir.path().join("bots.db"))
        .await
        .expect("file database");
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("migrations");
    let f = fixture_on(db.clone()).await;
    let owner = human(&f, "rawkode").await;

    let mut creates = Vec::new();
    for index in 0..8 {
        let store = BotStore::new(DbActor::spawn(DbActor::new(db.clone())));
        let owner = owner.clone();
        creates.push(tokio::spawn(async move {
            let bot = bot_of(&owner, &format!("bot{index}"));
            store.create(&bot, &owner, None, 3).await.expect("create")
        }));
    }
    let mut created = 0;
    for create in creates {
        match create.await.expect("join") {
            CreateBotOutcome::Created => created += 1,
            CreateBotOutcome::LimitReached => {}
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    assert_eq!(created, 3);
    assert_eq!(f.store.list_for_owner(&owner).await.expect("list").len(), 3);
}

#[tokio::test]
async fn delete_cascades_tokens() {
    let f = fixture("bots-delete-cascades").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    mint(&f, &bot).await;

    assert!(f.store.delete(&bot).await.expect("delete"));
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM bot_tokens WHERE bot_jid = ?",
            vec![Value::from(bot.as_bare().as_str())],
        )
        .await,
        0
    );
    assert!(f.store.get(&bot).await.expect("get").is_none());
    assert!(matches!(
        f.tokens.validate(minted.secret.expose()).await,
        Err(BotTokenError::Unknown)
    ));
    assert!(!f.store.delete(&bot).await.expect("delete again"));
    // Deleting a bot never deletes a human row of the same JID.
    assert!(!f
        .store
        .delete(&BotJid::from_storage(jid(&format!("rawkode+x@{DOMAIN}"))).expect("bot jid"))
        .await
        .expect("delete missing"));
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM users WHERE jid = ?",
            vec![Value::from(owner.as_str())],
        )
        .await,
        1
    );
}

#[tokio::test]
async fn list_for_owner_only_returns_own() {
    let f = fixture("bots-list-for-owner").await;
    let rawkode = human(&f, "rawkode").await;
    let alice = human(&f, "alice").await;
    let first = create(&f, &rawkode, "standup").await;
    let second = create(&f, &rawkode, "ci").await;
    create(&f, &alice, "standup").await;

    let listed: Vec<BotJid> = f
        .store
        .list_for_owner(&rawkode)
        .await
        .expect("list")
        .into_iter()
        .map(|record| {
            assert_eq!(record.owner, rawkode);
            record.jid
        })
        .collect();
    assert_eq!(listed, vec![first, second]);
    assert!(f
        .store
        .list_for_owner(&jid("nobody@waddle.social"))
        .await
        .expect("list empty")
        .is_empty());
}

#[tokio::test]
async fn mint_returns_prefixed_47_char_secret() {
    let f = fixture("bots-mint-secret").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = f
        .tokens
        .mint(
            &bot,
            label("  deploy  "),
            BotTokenTtl::days(7).expect("ttl"),
        )
        .await
        .expect("mint");

    let secret = minted.secret.expose();
    assert!(secret.starts_with(BOT_TOKEN_PREFIX));
    assert_eq!(secret.len(), 47);
    assert!(secret[BOT_TOKEN_PREFIX.len()..]
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
    assert_ne!(mint(&f, &bot).await.secret.expose(), secret);

    assert_eq!(minted.summary.label.as_str(), "deploy");
    assert_eq!(
        minted.summary.expires_at - minted.summary.created_at,
        Duration::days(7)
    );
    let listed = f.tokens.list_for_bot(&bot).await.expect("list tokens");
    assert_eq!(listed.first(), Some(&minted.summary));
    assert_eq!(listed.len(), 2);
}

#[tokio::test]
async fn stored_hash_is_not_plaintext() {
    let f = fixture("bots-stored-hash").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    let secret = minted.secret.expose();

    let rows = query(
        &f,
        "SELECT id, bot_jid, token_hash, label, created_at, last_used_at, expires_at, \
         revoked_at FROM bot_tokens",
        vec![],
    )
    .await;
    assert_eq!(rows.len(), 1);
    for value in &rows[0] {
        if let Value::Text(text) = value {
            assert!(!text.contains(secret));
            assert!(!text.contains(&secret[BOT_TOKEN_PREFIX.len()..]));
        }
    }
    assert_eq!(
        rows[0][2],
        Value::Text(TokenHasher::new(Some(SESSION_KEY)).bot_token(secret))
    );
}

#[tokio::test]
async fn validate_roundtrip_returns_principal() {
    let f = fixture("bots-validate-roundtrip").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;

    let principal = f
        .tokens
        .validate(minted.secret.expose())
        .await
        .expect("validate");
    assert_eq!(
        principal,
        BotPrincipal {
            bot_jid: bot.as_bare().clone(),
            owner_jid: owner,
            token_id: minted.summary.id,
        }
    );
}

#[tokio::test]
async fn validate_rejects_session_uuid_as_malformed() {
    let f = fixture("bots-validate-malformed").await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let valid_body = "A".repeat(43);
    for presented in [
        session_id.clone(),
        String::new(),
        format!("wbt{valid_body}x"),
        format!("{BOT_TOKEN_PREFIX}{}", "A".repeat(42)),
        format!("{BOT_TOKEN_PREFIX}{}", "A".repeat(44)),
        format!("{BOT_TOKEN_PREFIX}{}+", "A".repeat(42)),
        format!("{BOT_TOKEN_PREFIX}{}=", "A".repeat(42)),
        format!("WBT_{valid_body}"),
    ] {
        assert!(
            matches!(
                f.tokens.validate(&presented).await,
                Err(BotTokenError::Malformed)
            ),
            "{presented:?}"
        );
    }
    // Rejected before any storage access: a dead database changes nothing.
    f.actor.kill();
    f.actor.wait_for_shutdown().await;
    assert!(matches!(
        f.tokens.validate(&session_id).await,
        Err(BotTokenError::Malformed)
    ));
}

#[tokio::test]
async fn validate_unknown() {
    let f = fixture("bots-validate-unknown").await;
    assert!(matches!(
        f.tokens.validate(&fake_secret('Z')).await,
        Err(BotTokenError::Unknown)
    ));
}

#[tokio::test]
async fn validate_revoked() {
    let f = fixture("bots-validate-revoked").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    assert_eq!(
        f.tokens
            .revoke(&bot, minted.summary.id)
            .await
            .expect("revoke"),
        RevokeOutcome::Revoked
    );
    assert!(matches!(
        f.tokens.validate(minted.secret.expose()).await,
        Err(BotTokenError::Revoked)
    ));
}

#[tokio::test]
async fn validate_expired() {
    let f = fixture("bots-validate-expired").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let secret = fake_secret('E');
    insert_token(&f, &bot, &secret, Utc::now() - Duration::seconds(1), None).await;
    assert!(matches!(
        f.tokens.validate(&secret).await,
        Err(BotTokenError::Expired)
    ));
}

#[tokio::test]
async fn validate_disabled_bot() {
    let f = fixture("bots-validate-disabled").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    execute(
        &f,
        "UPDATE users SET disabled_at = ?, disabled_reason = 'owner_not_member' WHERE jid = ?",
        vec![
            Value::from(storage_timestamp(Utc::now())),
            Value::from(bot.as_bare().as_str()),
        ],
    )
    .await;
    assert!(matches!(
        f.tokens.validate(minted.secret.expose()).await,
        Err(BotTokenError::BotDisabled)
    ));
    assert!(!f.store.exists_enabled(bot.as_bare()).await.expect("exists"));
    let record = f.store.get(&bot).await.expect("get").expect("bot");
    assert!(matches!(
        record.disabled,
        Some((_, DisabledReason::OwnerNotMember))
    ));
}

#[tokio::test]
async fn validate_touch_is_throttled() {
    let f = fixture("bots-validate-touch").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    let id = minted.summary.id;
    assert_eq!(last_used_at(&f, id).await, None);

    f.tokens
        .validate(minted.secret.expose())
        .await
        .expect("first validate");
    let first = last_used_at(&f, id).await.expect("touched");
    f.tokens
        .validate(minted.secret.expose())
        .await
        .expect("second validate");
    assert_eq!(last_used_at(&f, id).await.as_deref(), Some(first.as_str()));

    // Older than the throttle window: the next validation records it.
    let stale = storage_timestamp(Utc::now() - Duration::seconds(61));
    execute(
        &f,
        "UPDATE bot_tokens SET last_used_at = ? WHERE id = ?",
        vec![Value::from(stale.as_str()), Value::from(id.to_string())],
    )
    .await;
    f.tokens
        .validate(minted.secret.expose())
        .await
        .expect("third validate");
    let third = last_used_at(&f, id).await.expect("touched again");
    assert!(third > stale, "{third} should be newer than {stale}");
}

#[tokio::test]
async fn revoke_scoped_by_bot() {
    let f = fixture("bots-revoke-scoped").await;
    let owner = member(&f, "rawkode").await;
    let standup = create(&f, &owner, "standup").await;
    let ci = create(&f, &owner, "ci").await;
    let minted = mint(&f, &standup).await;

    assert_eq!(
        f.tokens
            .revoke(&ci, minted.summary.id)
            .await
            .expect("revoke under another bot"),
        RevokeOutcome::NotFound
    );
    assert_eq!(
        f.tokens
            .revoke(&standup, BotTokenId::new())
            .await
            .expect("revoke unknown id"),
        RevokeOutcome::NotFound
    );
    f.tokens
        .validate(minted.secret.expose())
        .await
        .expect("token still valid");
}

#[tokio::test]
async fn revoke_twice_already_revoked() {
    let f = fixture("bots-revoke-twice").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    assert_eq!(
        f.tokens
            .revoke(&bot, minted.summary.id)
            .await
            .expect("revoke"),
        RevokeOutcome::Revoked
    );
    assert_eq!(
        f.tokens
            .revoke(&bot, minted.summary.id)
            .await
            .expect("revoke again"),
        RevokeOutcome::AlreadyRevoked
    );
    let listed = f.tokens.list_for_bot(&bot).await.expect("list");
    assert!(listed[0].revoked_at.is_some());
}

#[tokio::test]
async fn sweep_removes_expired_and_old_revoked_keeps_recent() {
    let f = fixture("bots-sweep").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let now = Utc::now();
    let expired = insert_token(&f, &bot, &fake_secret('A'), now - Duration::hours(1), None).await;
    let old_revoked = insert_token(
        &f,
        &bot,
        &fake_secret('B'),
        now + Duration::days(30),
        Some(now - Duration::days(8)),
    )
    .await;
    let recent_revoked = insert_token(
        &f,
        &bot,
        &fake_secret('C'),
        now + Duration::days(30),
        Some(now - Duration::days(6)),
    )
    .await;
    let active = insert_token(&f, &bot, &fake_secret('D'), now + Duration::days(30), None).await;

    assert_eq!(
        f.tokens.sweep(now, Duration::days(7)).await.expect("sweep"),
        2
    );
    let remaining: Vec<BotTokenId> = f
        .tokens
        .list_for_bot(&bot)
        .await
        .expect("list")
        .into_iter()
        .map(|summary| summary.id)
        .collect();
    assert!(remaining.contains(&recent_revoked));
    assert!(remaining.contains(&active));
    assert!(!remaining.contains(&expired));
    assert!(!remaining.contains(&old_revoked));
    assert_eq!(remaining.len(), 2);
}

#[tokio::test]
async fn bot_token_not_accepted_by_validate_session() {
    let f = fixture("bots-token-not-session").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    let sessions = SessionManager::new(f.actor.clone(), Some(SESSION_KEY));
    assert!(matches!(
        sessions.validate_session(minted.secret.expose()).await,
        Err(AuthError::SessionNotFound(_))
    ));
}

#[tokio::test]
async fn error_display_never_contains_token() {
    let f = fixture("bots-error-display").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    let secret = minted.secret.expose().to_owned();
    let body = &secret[BOT_TOKEN_PREFIX.len()..];

    f.tokens
        .revoke(&bot, minted.summary.id)
        .await
        .expect("revoke");
    let mut errors = vec![
        BotTokenError::Malformed,
        BotTokenError::Unknown,
        BotTokenError::Revoked,
        BotTokenError::Expired,
        BotTokenError::BotDisabled,
        BotTokenError::OwnerNotMember,
        BotTokenError::TokenLimitReached,
        BotTokenError::NotFound,
        BotTokenError::Unavailable,
        BotTokenError::Storage("database is locked".to_string()),
    ];
    errors.push(f.tokens.validate(&secret).await.expect_err("revoked"));
    errors.push(
        f.tokens
            .validate(&format!("{secret}x"))
            .await
            .expect_err("malformed"),
    );
    f.actor.kill();
    f.actor.wait_for_shutdown().await;
    errors.push(
        f.tokens
            .validate(&secret)
            .await
            .expect_err("storage failure"),
    );

    for error in errors {
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains(body), "{rendered}");
        }
    }
}

#[tokio::test]
async fn mint_enforces_active_token_cap() {
    let f = fixture("bots-mint-cap").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let mut minted = Vec::new();
    for _ in 0..MAX_ACTIVE_TOKENS_PER_BOT {
        minted.push(mint(&f, &bot).await);
    }
    assert!(matches!(
        f.tokens
            .mint(&bot, label("eleventh"), BotTokenTtl::default())
            .await,
        Err(BotTokenError::TokenLimitReached)
    ));

    // A revoked token frees its slot.
    f.tokens
        .revoke(&bot, minted[0].summary.id)
        .await
        .expect("revoke");
    mint(&f, &bot).await;
    assert!(matches!(
        f.tokens
            .mint(&bot, label("full"), BotTokenTtl::default())
            .await,
        Err(BotTokenError::TokenLimitReached)
    ));

    // So does an expired one.
    execute(
        &f,
        "UPDATE bot_tokens SET expires_at = ? WHERE id = ?",
        vec![
            Value::from(storage_timestamp(Utc::now() - Duration::seconds(1))),
            Value::from(minted[1].summary.id.to_string()),
        ],
    )
    .await;
    mint(&f, &bot).await;
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM bot_tokens WHERE bot_jid = ?",
            vec![Value::from(bot.as_bare().as_str())],
        )
        .await,
        i64::from(MAX_ACTIVE_TOKENS_PER_BOT) + 2
    );
}

#[tokio::test]
async fn mint_for_disabled_bot_is_bot_disabled() {
    let f = fixture("bots-mint-disabled").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    execute(
        &f,
        "UPDATE users SET disabled_at = ?, disabled_reason = 'owner_missing' WHERE jid = ?",
        vec![
            Value::from(storage_timestamp(Utc::now())),
            Value::from(bot.as_bare().as_str()),
        ],
    )
    .await;
    assert!(matches!(
        f.tokens
            .mint(&bot, label("ci"), BotTokenTtl::default())
            .await,
        Err(BotTokenError::BotDisabled)
    ));
    assert!(matches!(
        f.tokens
            .mint(
                &bot_of(&owner, "missing"),
                label("ci"),
                BotTokenTtl::default()
            )
            .await,
        Err(BotTokenError::NotFound)
    ));
    assert!(f.tokens.list_for_bot(&bot).await.expect("list").is_empty());
}

#[tokio::test]
async fn validate_rejects_when_owner_lost_membership() {
    let f = fixture("bots-validate-owner-gate").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;
    f.tokens
        .validate(minted.secret.expose())
        .await
        .expect("valid while a member");

    f.permissions
        .ask(DeleteTuple {
            tuple: server_member(&owner),
        })
        .await
        .expect("remove membership");
    // No janitor runs: the live gate alone rejects the token.
    assert!(matches!(
        f.tokens.validate(minted.secret.expose()).await,
        Err(BotTokenError::OwnerNotMember)
    ));
}

#[tokio::test]
async fn validate_permission_backend_error_is_unavailable() {
    let f = fixture("bots-validate-backend-error").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;
    let minted = mint(&f, &bot).await;

    f.permissions.kill();
    f.permissions.wait_for_shutdown().await;
    assert!(matches!(
        f.tokens.validate(minted.secret.expose()).await,
        Err(BotTokenError::Unavailable)
    ));
}

fn channel() -> Object {
    Object::new(ObjectType::Channel, "general@muc.waddle.social")
}

fn principal_for(owner: &BareJid, bot: &BotJid) -> BotPrincipal {
    BotPrincipal {
        bot_jid: bot.as_bare().clone(),
        owner_jid: owner.clone(),
        token_id: BotTokenId::new(),
    }
}

#[tokio::test]
async fn authorize_bot_requires_owner_and_bot() {
    for (bot_allowed, owner_allowed, expected) in [
        (true, true, true),
        (true, false, false),
        (false, true, false),
        (false, false, false),
    ] {
        let f = fixture("bots-authorize-table").await;
        let owner = jid("rawkode@waddle.social");
        let bot = bot_of(&owner, "standup");
        if bot_allowed {
            write(
                &f,
                Tuple::new(
                    channel(),
                    Relation::new("member"),
                    Subject::user(bot.to_string()),
                ),
            )
            .await;
        }
        if owner_allowed {
            write(
                &f,
                Tuple::new(
                    channel(),
                    Relation::new("member"),
                    Subject::user(owner.to_string()),
                ),
            )
            .await;
        }
        for permission in [Permission::Read, Permission::SendMessage] {
            assert_eq!(
                authorize_bot(
                    &f.permissions,
                    &principal_for(&owner, &bot),
                    permission.clone(),
                    &channel()
                )
                .await
                .expect("authorize"),
                expected,
                "bot={bot_allowed} owner={owner_allowed} {permission:?}"
            );
        }
    }
}

/// A channel inside a space where the owner and the bot are both members,
/// with the bot also holding an explicit channel grant. Space membership
/// alone grants channel `read`/`send_message` through `parent->view`.
async fn channel_in_space_with_members(f: &Fixture, owner: &BareJid, bot: &BotJid) {
    const SPACE: &str = "penguin-club";
    write(
        f,
        Tuple::new(
            channel(),
            Relation::new("parent"),
            Subject::userset(SubjectType::Space, SPACE, ""),
        ),
    )
    .await;
    for subject in [owner.to_string(), bot.to_string()] {
        write(
            f,
            Tuple::new(
                Object::new(ObjectType::Space, SPACE),
                Relation::new("member"),
                Subject::user(subject),
            ),
        )
        .await;
    }
    write(
        f,
        Tuple::new(
            channel(),
            Relation::new("member"),
            Subject::user(bot.to_string()),
        ),
    )
    .await;
}

async fn outcast(f: &Fixture, subject: String) {
    f.permissions
        .ask(ReplaceExclusiveRelation {
            object: channel(),
            subject: Subject::user(subject),
            family: ["owner", "admin", "member", "outcast"]
                .into_iter()
                .map(Relation::new)
                .collect(),
            replacement: Some(Relation::new("outcast")),
        })
        .await
        .expect("outcast");
}

async fn assert_bot_channel_access(f: &Fixture, principal: &BotPrincipal, expected: bool) {
    for permission in [Permission::Read, Permission::SendMessage] {
        assert_eq!(
            authorize_bot(&f.permissions, principal, permission.clone(), &channel())
                .await
                .expect("authorize"),
            expected,
            "{permission:?}"
        );
    }
}

#[tokio::test]
async fn authorize_bot_denied_after_owner_outcast_from_channel() {
    let f = fixture("bots-authorize-owner-outcast").await;
    let owner = jid("rawkode@waddle.social");
    let bot = bot_of(&owner, "standup");
    let principal = principal_for(&owner, &bot);
    channel_in_space_with_members(&f, &owner, &bot).await;
    assert_bot_channel_access(&f, &principal, true).await;

    outcast(&f, owner.to_string()).await;
    // The owner still reaches the channel through the space, but an outcast
    // owner takes the bot's access with them.
    assert!(
        f.permissions
            .ask(CheckPermission {
                subject: Subject::user(owner.to_string()),
                permission: Permission::Read,
                object: channel(),
            })
            .await
            .expect("owner read")
            .allowed,
        "fixture must keep parent->view for the outcast owner"
    );
    assert_bot_channel_access(&f, &principal, false).await;
}

#[tokio::test]
async fn authorize_bot_denied_after_bot_outcast_from_channel() {
    let f = fixture("bots-authorize-bot-outcast").await;
    let owner = jid("rawkode@waddle.social");
    let bot = bot_of(&owner, "standup");
    let principal = principal_for(&owner, &bot);
    channel_in_space_with_members(&f, &owner, &bot).await;
    assert_bot_channel_access(&f, &principal, true).await;

    outcast(&f, bot.to_string()).await;
    assert_bot_channel_access(&f, &principal, false).await;
}

/// The transactional create and mint paths on Postgres. Skipped unless
/// `WADDLE_TEST_POSTGRES_URL` is set.
#[tokio::test]
async fn postgres_bots_create_mint_and_validate() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (postgres bot store)");
        return;
    };
    let db = Database::from_config(
        "bots-postgres",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url),
    )
    .await
    .expect("postgres database");
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("migrations");
    let f = fixture_on(db).await;
    let run = uuid::Uuid::new_v4().simple().to_string();
    let owner = member(&f, &format!("owner{run}")).await;

    let bot = bot_of(&owner, "standup");
    assert_eq!(
        f.store
            .create(&bot, &owner, Some("Standup".to_string()), 2)
            .await
            .expect("create"),
        CreateBotOutcome::Created
    );
    assert_eq!(
        f.store
            .create(&bot, &owner, None, 2)
            .await
            .expect("recreate"),
        CreateBotOutcome::AlreadyExists { same_owner: true }
    );
    create_with_limit(&f, &owner, "ci", 2, CreateBotOutcome::Created).await;
    create_with_limit(&f, &owner, "third", 2, CreateBotOutcome::LimitReached).await;

    let minted = mint(&f, &bot).await;
    let principal = f
        .tokens
        .validate(minted.secret.expose())
        .await
        .expect("validate");
    assert_eq!(principal.owner_jid, owner);
    for _ in 1..MAX_ACTIVE_TOKENS_PER_BOT {
        mint(&f, &bot).await;
    }
    assert!(matches!(
        f.tokens
            .mint(&bot, label("over"), BotTokenTtl::default())
            .await,
        Err(BotTokenError::TokenLimitReached)
    ));

    for record in f.store.list_for_owner(&owner).await.expect("list") {
        assert!(f.store.delete(&record.jid).await.expect("delete"));
    }
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM bot_tokens WHERE bot_jid = ?",
            vec![Value::from(bot.as_bare().as_str())],
        )
        .await,
        0
    );
    execute(
        &f,
        "DELETE FROM users WHERE jid = ?",
        vec![Value::from(owner.as_str())],
    )
    .await;
}

async fn create_with_limit(
    f: &Fixture,
    owner: &BareJid,
    bot: &str,
    limit: u32,
    expected: CreateBotOutcome,
) {
    assert_eq!(
        f.store
            .create(&bot_of(owner, bot), owner, None, limit)
            .await
            .expect("create"),
        expected
    );
}

#[tokio::test]
async fn bot_account_exists_requires_enabled_bot_row() {
    let f = fixture("bots-account-exists").await;
    let owner = member(&f, "rawkode").await;
    let bot = create(&f, &owner, "standup").await;

    assert!(store::bot_account_exists(&f.actor, bot.as_bare())
        .await
        .expect("lookup"));
    // The owner's human row is not a bot account, on either domain.
    assert!(!store::bot_account_exists(&f.actor, &owner)
        .await
        .expect("lookup"));
    assert!(
        !store::bot_account_exists(&f.actor, &jid("rawkode@bots.waddle.social"))
            .await
            .expect("lookup")
    );
    assert!(
        !store::bot_account_exists(&f.actor, &jid("rawkode+ghost@bots.waddle.social"))
            .await
            .expect("lookup")
    );

    execute(
        &f,
        "UPDATE users SET disabled_at = ?, disabled_reason = 'owner_missing' WHERE jid = ?",
        vec![
            Value::from(storage_timestamp(Utc::now())),
            Value::from(bot.as_bare().as_str()),
        ],
    )
    .await;
    assert!(!store::bot_account_exists(&f.actor, bot.as_bare())
        .await
        .expect("lookup"));
}
