use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::Utc;
use jid::{BareJid, DomainPart};
use kameo::actor::{ActorRef, Spawn};

use super::*;
use crate::auth::bots::{
    storage_timestamp, BotJid, BotName, BotTokenError, BotTokenLabel, BotTokenSecret, BotTokenTtl,
};
use crate::auth::token_hash::TokenHasher;
use crate::db::actor::{
    CreateBotOutcome, DbActor, DbExecute, DbQueryOne, DisableBot, DisableBotReport, EnableBot,
};
use crate::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner, Value};
use crate::permissions::{DeleteTuple, Relation, Tuple, WriteTuple};
use crate::server::bootstrap_membership::{
    reconcile_user_membership, BootstrapMembershipConfig, DEPLOYMENT_SERVER_ID,
};

const DOMAIN: &str = "waddle.social";

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
    fixture_on(db)
}

fn fixture_on(db: Database) -> Fixture {
    let actor = DbActor::spawn(DbActor::new(db.clone()));
    let permissions = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(db)));
    Fixture {
        store: BotStore::new(actor.clone()),
        tokens: BotTokenStore::new(
            actor.clone(),
            permissions.clone(),
            DEPLOYMENT_SERVER_ID.to_string(),
            TokenHasher::new(Some(b"test-session-key")),
        ),
        actor,
        permissions,
    }
}

impl Fixture {
    fn membership(&self) -> PermissionMembership<'_> {
        PermissionMembership {
            permissions: &self.permissions,
            server_id: DEPLOYMENT_SERVER_ID,
        }
    }

    /// The production limits, with this fixture's stores and membership.
    fn sweep(&self) -> Sweep<'_, PermissionMembership<'_>> {
        self.sweep_with(
            self.membership(),
            BOT_LIFECYCLE_PAGE,
            BOT_JANITOR_TICK_BUDGET,
        )
    }

    fn sweep_with<M: OwnerMembership>(
        &self,
        membership: M,
        page_size: u32,
        tick_budget: Duration,
    ) -> Sweep<'_, M> {
        Sweep {
            store: &self.store,
            tokens: &self.tokens,
            membership,
            page_size,
            page_timeout: BOT_JANITOR_PAGE_TIMEOUT,
            tick_budget,
        }
    }

    async fn execute(&self, sql: &str, params: Vec<Value>) -> u64 {
        self.actor
            .ask(DbExecute {
                sql: sql.to_string(),
                params,
            })
            .await
            .expect("execute")
    }

    async fn one(&self, sql: &str, params: Vec<Value>) -> Vec<Value> {
        self.actor
            .ask(DbQueryOne {
                sql: sql.to_string(),
                params,
            })
            .await
            .expect("query")
            .expect("row")
    }

    /// A human `users` row with no credential and no membership.
    async fn human(&self, localpart: &str) -> BareJid {
        let owner = jid(&format!("{localpart}@{DOMAIN}"));
        let now = Utc::now().to_rfc3339();
        self.execute(
            "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, created_at, \
             updated_at) VALUES (?, ?, ?, ?, ?, ?)",
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

    async fn oidc_identity(&self, owner: &BareJid) {
        let now = Utc::now().to_rfc3339();
        self.execute(
            "INSERT INTO auth_identities (id, user_jid, provider_id, issuer, subject, \
             raw_claims_json, created_at, last_login_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                Value::from(uuid::Uuid::new_v4().to_string()),
                Value::from(owner.as_str()),
                Value::from("test"),
                Value::from("https://issuer.test"),
                Value::from(owner.as_str()),
                Value::from("{}"),
                Value::from(now.as_str()),
                Value::from(now.as_str()),
            ],
        )
        .await;
    }

    async fn native_account(&self, owner: &BareJid) {
        let node = owner.node().map(|node| node.as_str()).unwrap_or_default();
        self.execute(
            "INSERT INTO native_users (username, domain, jid_key, password_hash, salt, \
             stored_key, server_key) VALUES (?, ?, ?, ?, ?, ?, ?)",
            vec![
                Value::from(node),
                Value::from(owner.domain().as_str()),
                Value::from(owner.as_str()),
                Value::from("hash"),
                Value::from("salt"),
                Value::Blob(vec![1]),
                Value::Blob(vec![2]),
            ],
        )
        .await;
    }

    async fn write(&self, tuple: Tuple) {
        self.permissions
            .ask(WriteTuple { tuple })
            .await
            .expect("write tuple");
    }

    /// A healthy owner: human row, OIDC identity and `server#member`.
    async fn member(&self, localpart: &str) -> BareJid {
        let owner = self.human(localpart).await;
        self.oidc_identity(&owner).await;
        self.write(server_member(&owner)).await;
        owner
    }

    async fn remove_membership(&self, owner: &BareJid) {
        self.permissions
            .ask(DeleteTuple {
                tuple: server_member(owner),
            })
            .await
            .expect("delete membership");
    }

    async fn bot(&self, owner: &BareJid, name: &str) -> BotJid {
        let bot = bot_of(owner, name);
        assert_eq!(
            self.store
                .create(&bot, owner, None, 100)
                .await
                .expect("create bot"),
            CreateBotOutcome::Created
        );
        bot
    }

    async fn mint(&self, bot: &BotJid) -> BotTokenSecret {
        self.tokens
            .mint(
                bot,
                BotTokenLabel::try_from("ci").expect("label"),
                BotTokenTtl::default(),
            )
            .await
            .expect("mint")
            .secret
    }

    /// `(disabled_at, disabled_reason)` of `bot`.
    async fn disabled(&self, bot: &BotJid) -> (Option<String>, Option<String>) {
        let row = self
            .one(
                "SELECT disabled_at, disabled_reason FROM users WHERE jid = ?",
                vec![Value::from(bot.as_bare().as_str())],
            )
            .await;
        (text(&row[0]), text(&row[1]))
    }

    async fn unrevoked_tokens(&self, bot: &BotJid) -> i64 {
        let row = self
            .one(
                "SELECT COUNT(*) FROM bot_tokens WHERE bot_jid = ? AND revoked_at IS NULL",
                vec![Value::from(bot.as_bare().as_str())],
            )
            .await;
        integer(&row[0])
    }

    async fn token_count(&self, bot: &BotJid) -> i64 {
        let row = self
            .one(
                "SELECT COUNT(*) FROM bot_tokens WHERE bot_jid = ?",
                vec![Value::from(bot.as_bare().as_str())],
            )
            .await;
        integer(&row[0])
    }

    /// Inserts a token row directly, for timestamps `mint` never writes.
    async fn insert_token(
        &self,
        bot: &BotJid,
        hash: &str,
        expires_at: DateTime<Utc>,
        revoked_at: Option<DateTime<Utc>>,
    ) {
        self.execute(
            "INSERT INTO bot_tokens (id, bot_jid, token_hash, label, created_at, last_used_at, \
             expires_at, revoked_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                Value::from(uuid::Uuid::new_v4().to_string()),
                Value::from(bot.as_bare().as_str()),
                Value::from(hash),
                Value::from("direct"),
                Value::from(storage_timestamp(Utc::now() - chrono::Duration::days(30))),
                Value::NullText,
                Value::from(storage_timestamp(expires_at)),
                Value::from(revoked_at.map(storage_timestamp)),
            ],
        )
        .await;
    }
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        other if other.is_null() => None,
        other => panic!("expected text, got {other:?}"),
    }
}

fn integer(value: &Value) -> i64 {
    match value {
        Value::Integer(value) => *value,
        other => panic!("expected an integer, got {other:?}"),
    }
}

fn jid(value: &str) -> BareJid {
    value.parse().expect("bare jid")
}

fn bots_domain() -> DomainPart {
    DomainPart::new(&format!("bots.{DOMAIN}"))
        .expect("bots domain")
        .into_owned()
}

fn bot_of(owner: &BareJid, name: &str) -> BotJid {
    BotJid::new(
        owner,
        &BotName::try_from(name).expect("bot name"),
        &bots_domain(),
    )
    .expect("bot jid")
}

fn server_member(user: &BareJid) -> Tuple {
    Tuple::new(
        Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        Relation::new("member"),
        Subject::user(user.to_string()),
    )
}

/// Counts every membership check it forwards.
struct CountingMembership<'a> {
    inner: PermissionMembership<'a>,
    calls: &'a AtomicUsize,
}

impl OwnerMembership for CountingMembership<'_> {
    async fn is_member(&self, owner: &BareJid) -> Result<bool, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.is_member(owner).await
    }
}

/// A permission backend that always fails.
struct FailingMembership;

impl OwnerMembership for FailingMembership {
    async fn is_member(&self, _owner: &BareJid) -> Result<bool, String> {
        Err("permission backend unavailable".to_string())
    }
}

fn health_row(
    owner_row_exists: bool,
    owner_has_credential: bool,
    disabled_reason: Option<DisabledReason>,
) -> BotOwnerHealthRow {
    let owner = jid("rawkode@waddle.social");
    BotOwnerHealthRow {
        bot: bot_of(&owner, "standup"),
        owner,
        disabled_reason,
        owner_row_exists,
        owner_has_credential,
    }
}

#[test]
fn bot_janitor_decide_table() {
    use DisabledReason::{OwnerMissing, OwnerNotMember};
    let reasons = [None, Some(OwnerMissing), Some(OwnerNotMember)];
    let memberships = [None, Some(false), Some(true)];
    for current in reasons {
        for member in memberships {
            // The owner is missing whatever membership says.
            for (row_exists, credential) in [(false, false), (false, true), (true, false)] {
                let expected = if current == Some(OwnerMissing) {
                    Transition::None
                } else {
                    Transition::Disable(OwnerMissing)
                };
                assert_eq!(
                    decide(&health_row(row_exists, credential, current), member),
                    expected,
                    "row={row_exists} credential={credential} current={current:?} member={member:?}"
                );
            }

            let expected = match (member, current) {
                // Unknown membership never changes anything.
                (None, _) => Transition::None,
                (Some(false), Some(OwnerNotMember)) => Transition::None,
                (Some(false), _) => Transition::Disable(OwnerNotMember),
                (Some(true), Some(_)) => Transition::Enable,
                (Some(true), None) => Transition::None,
            };
            assert_eq!(
                decide(&health_row(true, true, current), member),
                expected,
                "healthy owner row, current={current:?} member={member:?}"
            );
        }
    }
}

#[tokio::test]
async fn bot_janitor_sweep_deletes_expired_and_old_revoked_tokens() {
    let f = fixture("bot-janitor-token-sweep").await;
    let owner = f.member("rawkode").await;
    let bot = f.bot(&owner, "standup").await;
    let now = Utc::now();
    let day = chrono::Duration::days(1);
    f.insert_token(&bot, "expired", now - day, None).await;
    f.insert_token(&bot, "revoked-old", now + day, Some(now - day * 8))
        .await;
    f.insert_token(&bot, "revoked-recent", now + day, Some(now - day))
        .await;
    f.insert_token(&bot, "active", now + day, None).await;

    let report = f.sweep().run(&mut None, now).await.expect("sweep");

    assert_eq!(report.tokens_deleted, 2);
    assert_eq!(report.outcome, SweepOutcome::Completed);
    let remaining = f
        .one(
            "SELECT COUNT(*) FROM bot_tokens WHERE token_hash IN ('revoked-recent', 'active')",
            Vec::new(),
        )
        .await;
    assert_eq!(integer(&remaining[0]), 2);
    assert_eq!(f.token_count(&bot).await, 2);
    // A healthy owner's bot is left alone.
    assert_eq!(f.disabled(&bot).await, (None, None));
    assert_eq!(f.unrevoked_tokens(&bot).await, 1);
}

#[tokio::test]
async fn bot_janitor_owner_row_missing_disables_and_revokes() {
    let f = fixture("bot-janitor-owner-row-missing").await;
    let owner = f.member("rawkode").await;
    let bot = f.bot(&owner, "standup").await;
    let secret = f.mint(&bot).await;
    f.mint(&bot).await;
    // The owner's row goes; nothing cascades to the bot (D4).
    f.execute(
        "DELETE FROM users WHERE jid = ?",
        vec![Value::from(owner.as_str())],
    )
    .await;

    let report = f.sweep().run(&mut None, Utc::now()).await.expect("sweep");

    assert_eq!(report.disabled, 1);
    assert_eq!(report.tokens_revoked, 2);
    let (disabled_at, reason) = f.disabled(&bot).await;
    assert!(disabled_at.is_some());
    assert_eq!(reason.as_deref(), Some("owner_missing"));
    assert_eq!(f.unrevoked_tokens(&bot).await, 0);
    assert!(matches!(
        f.tokens.validate(secret.expose()).await,
        Err(BotTokenError::Revoked)
    ));
}

#[tokio::test]
async fn bot_janitor_owner_without_credentials_disables() {
    let f = fixture("bot-janitor-owner-no-credentials").await;
    let owner = f.member("rawkode").await;
    let bot = f.bot(&owner, "standup").await;
    f.mint(&bot).await;
    f.execute(
        "DELETE FROM auth_identities WHERE user_jid = ?",
        vec![Value::from(owner.as_str())],
    )
    .await;

    let calls = AtomicUsize::new(0);
    let membership = CountingMembership {
        inner: f.membership(),
        calls: &calls,
    };
    let report = f
        .sweep_with(membership, BOT_LIFECYCLE_PAGE, BOT_JANITOR_TICK_BUDGET)
        .run(&mut None, Utc::now())
        .await
        .expect("sweep");

    assert_eq!(report.disabled, 1);
    assert_eq!(report.tokens_revoked, 1);
    assert_eq!(f.disabled(&bot).await.1.as_deref(), Some("owner_missing"));
    assert_eq!(f.unrevoked_tokens(&bot).await, 0);
    // A missing owner needs no permission check.
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn bot_janitor_owner_not_member_disables_and_revokes() {
    let f = fixture("bot-janitor-owner-not-member").await;
    let owner = f.member("rawkode").await;
    let bot = f.bot(&owner, "standup").await;
    let secret = f.mint(&bot).await;
    f.remove_membership(&owner).await;

    let report = f.sweep().run(&mut None, Utc::now()).await.expect("sweep");

    assert_eq!(report.disabled, 1);
    assert_eq!(report.tokens_revoked, 1);
    assert_eq!(
        f.disabled(&bot).await.1.as_deref(),
        Some("owner_not_member")
    );
    assert_eq!(f.unrevoked_tokens(&bot).await, 0);
    assert!(matches!(
        f.tokens.validate(secret.expose()).await,
        Err(BotTokenError::Revoked)
    ));

    // A second sweep changes nothing.
    let again = f.sweep().run(&mut None, Utc::now()).await.expect("sweep");
    assert_eq!((again.disabled, again.tokens_revoked), (0, 0));
}

#[tokio::test]
async fn bot_janitor_owner_missing_takes_over_from_not_member() {
    let f = fixture("bot-janitor-reason-change").await;
    let owner = f.member("rawkode").await;
    let bot = f.bot(&owner, "standup").await;
    f.remove_membership(&owner).await;
    f.sweep().run(&mut None, Utc::now()).await.expect("sweep");
    let (first_disabled_at, _) = f.disabled(&bot).await;

    f.execute(
        "DELETE FROM auth_identities WHERE user_jid = ?",
        vec![Value::from(owner.as_str())],
    )
    .await;
    let report = f.sweep().run(&mut None, Utc::now()).await.expect("sweep");

    assert_eq!(report.disabled, 1);
    let (disabled_at, reason) = f.disabled(&bot).await;
    assert_eq!(reason.as_deref(), Some("owner_missing"));
    // The bot has been disabled since the first transition.
    assert_eq!(disabled_at, first_disabled_at);
}

#[tokio::test]
async fn bot_janitor_membership_restored_via_reconcile_reenables_without_tokens() {
    let f = fixture("bot-janitor-membership-restored").await;
    let owner = f.member("rawkode").await;
    let bot = f.bot(&owner, "standup").await;
    let secret = f.mint(&bot).await;
    f.remove_membership(&owner).await;
    f.sweep().run(&mut None, Utc::now()).await.expect("sweep");
    assert_eq!(
        f.disabled(&bot).await.1.as_deref(),
        Some("owner_not_member")
    );

    // The OIDC login path re-provisions membership.
    let primary: DomainPart = DOMAIN.parse().expect("primary domain");
    reconcile_user_membership(
        &f.permissions,
        &BootstrapMembershipConfig::new(Vec::new()),
        &primary,
        owner.as_str(),
        "rawkode",
    )
    .await
    .expect("reconcile membership");
    let report = f.sweep().run(&mut None, Utc::now()).await.expect("sweep");

    assert_eq!(report.enabled, 1);
    assert_eq!(f.disabled(&bot).await, (None, None));
    // The old token stays revoked; the owner mints a new one.
    assert!(matches!(
        f.tokens.validate(secret.expose()).await,
        Err(BotTokenError::Revoked)
    ));
    let fresh = f.mint(&bot).await;
    let principal = f
        .tokens
        .validate(fresh.expose())
        .await
        .expect("fresh token validates");
    assert_eq!(&principal.bot_jid, bot.as_bare());
}

#[tokio::test]
async fn bot_janitor_disable_and_revoke_are_atomic() {
    let f = fixture("bot-janitor-disable-atomic").await;
    let owner = f.member("rawkode").await;
    let bot = f.bot(&owner, "standup").await;
    f.mint(&bot).await;
    f.mint(&bot).await;

    let first = f
        .actor
        .ask(DisableBot {
            bot: bot.clone(),
            reason: DisabledReason::OwnerNotMember,
            now: Utc::now(),
        })
        .await
        .expect("disable");
    assert_eq!(
        first,
        DisableBotReport {
            disabled: true,
            tokens_revoked: 2,
        }
    );
    assert_eq!(
        f.disabled(&bot).await.1.as_deref(),
        Some("owner_not_member")
    );
    assert_eq!(f.unrevoked_tokens(&bot).await, 0);

    // An unrevoked token on an already-disabled bot is revoked on re-run,
    // and the users row is left as it was.
    let before = f.disabled(&bot).await;
    let now = Utc::now();
    f.insert_token(&bot, "leftover", now + chrono::Duration::days(1), None)
        .await;
    let again = f
        .actor
        .ask(DisableBot {
            bot: bot.clone(),
            reason: DisabledReason::OwnerNotMember,
            now,
        })
        .await
        .expect("disable again");
    assert_eq!(
        again,
        DisableBotReport {
            disabled: false,
            tokens_revoked: 1,
        }
    );
    assert_eq!(f.disabled(&bot).await, before);
    assert_eq!(f.unrevoked_tokens(&bot).await, 0);

    // Enabling leaves revoked tokens revoked.
    let enabled = f
        .actor
        .ask(EnableBot {
            bot: bot.clone(),
            now: Utc::now(),
        })
        .await
        .expect("enable");
    assert!(enabled);
    assert_eq!(f.disabled(&bot).await, (None, None));
    assert_eq!(f.unrevoked_tokens(&bot).await, 0);
    let enabled_again = f
        .actor
        .ask(EnableBot {
            bot: bot.clone(),
            now: Utc::now(),
        })
        .await
        .expect("enable again");
    assert!(!enabled_again);
}

#[tokio::test]
async fn bot_janitor_permission_error_does_not_disable() {
    let f = fixture("bot-janitor-permission-error").await;
    let owner = f.member("rawkode").await;
    let bot = f.bot(&owner, "standup").await;
    f.mint(&bot).await;

    // A real backend failure: the permission actor is gone.
    f.permissions.kill();
    f.permissions.wait_for_shutdown().await;
    let report = f.sweep().run(&mut None, Utc::now()).await.expect("sweep");

    assert_eq!(report.outcome, SweepOutcome::Failed);
    assert_eq!(report.evaluated, 1);
    assert_eq!((report.disabled, report.tokens_revoked), (0, 0));
    assert_eq!(f.disabled(&bot).await, (None, None));
    assert_eq!(f.unrevoked_tokens(&bot).await, 1);
}

#[tokio::test]
async fn bot_janitor_permission_error_does_not_reenable_or_block_missing_owners() {
    let f = fixture("bot-janitor-permission-error-mixed").await;
    let healthy = f.member("rawkode").await;
    let disabled = f.bot(&healthy, "standup").await;
    f.actor
        .ask(DisableBot {
            bot: disabled.clone(),
            reason: DisabledReason::OwnerNotMember,
            now: Utc::now(),
        })
        .await
        .expect("disable");
    let gone = f.member("gone").await;
    let orphan = f.bot(&gone, "ci").await;
    f.execute(
        "DELETE FROM users WHERE jid = ?",
        vec![Value::from(gone.as_str())],
    )
    .await;

    let report = f
        .sweep_with(
            FailingMembership,
            BOT_LIFECYCLE_PAGE,
            BOT_JANITOR_TICK_BUDGET,
        )
        .run(&mut None, Utc::now())
        .await
        .expect("sweep");

    assert_eq!(report.outcome, SweepOutcome::Failed);
    // The healthy owner's membership is unknown: the bot stays disabled.
    assert_eq!(
        f.disabled(&disabled).await.1.as_deref(),
        Some("owner_not_member")
    );
    // A missing owner needs no permission check and is still handled.
    assert_eq!(
        f.disabled(&orphan).await.1.as_deref(),
        Some("owner_missing")
    );
}

#[tokio::test]
async fn bot_janitor_membership_checked_once_per_owner() {
    let f = fixture("bot-janitor-membership-memo").await;
    let owner = f.member("rawkode").await;
    for name in ["a", "b", "c", "d", "e"] {
        f.bot(&owner, name).await;
    }

    let calls = AtomicUsize::new(0);
    let membership = CountingMembership {
        inner: f.membership(),
        calls: &calls,
    };
    // Pages of two, so the memo holds across pages within the tick.
    let report = f
        .sweep_with(membership, 2, BOT_JANITOR_TICK_BUDGET)
        .run(&mut None, Utc::now())
        .await
        .expect("sweep");

    assert_eq!(report.evaluated, 5);
    assert_eq!(report.outcome, SweepOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bot_janitor_cursor_resumes_across_ticks() {
    let f = fixture("bot-janitor-cursor").await;
    let owner = f.member("rawkode").await;
    for name in ["a", "b", "c", "d", "e"] {
        f.bot(&owner, name).await;
    }
    f.remove_membership(&owner).await;

    let mut cursor = None;
    let mut evaluated = 0;
    let mut outcomes = Vec::new();
    // A zero budget allows exactly one page per tick.
    for _ in 0..3 {
        let report = f
            .sweep_with(f.membership(), 2, Duration::ZERO)
            .run(&mut cursor, Utc::now())
            .await
            .expect("sweep");
        evaluated += report.evaluated;
        outcomes.push(report.outcome);
    }

    assert_eq!(evaluated, 5);
    assert_eq!(
        outcomes,
        [
            SweepOutcome::Deferred,
            SweepOutcome::Deferred,
            SweepOutcome::Completed
        ]
    );
    assert_eq!(cursor, None, "the last page resets the cursor");
    let disabled = f
        .one(
            "SELECT COUNT(*) FROM users WHERE kind = 'bot' AND disabled_reason = ?",
            vec![Value::from(DisabledReason::OwnerNotMember.as_str())],
        )
        .await;
    assert_eq!(integer(&disabled[0]), 5);
}

#[tokio::test]
async fn bot_janitor_records_one_sample_per_tick() {
    let f = fixture("bot-janitor-metrics").await;
    let owner = f.member("rawkode").await;
    f.bot(&owner, "standup").await;

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    f.sweep().run(&mut None, Utc::now()).await.expect("sweep");
    assert_eq!(
        metrics.counter_sum(
            "waddle.janitor.sweeps",
            &[("janitor", "bot_lifecycle"), ("outcome", "completed")]
        ),
        Some(1)
    );

    f.sweep_with(
        FailingMembership,
        BOT_LIFECYCLE_PAGE,
        BOT_JANITOR_TICK_BUDGET,
    )
    .run(&mut None, Utc::now())
    .await
    .expect("sweep");
    assert_eq!(
        metrics.counter_sum(
            "waddle.janitor.sweeps",
            &[("janitor", "bot_lifecycle"), ("outcome", "failed")]
        ),
        Some(1)
    );
    // Two ticks, two samples in all.
    assert_eq!(
        metrics.counter_sum("waddle.janitor.sweeps", &[("janitor", "bot_lifecycle")]),
        Some(2)
    );
}

#[tokio::test]
async fn bot_lifecycle_page_reports_owner_health() {
    let f = fixture("bot-lifecycle-page").await;
    let oidc = f.member("oidc").await;
    let native = f.human("native").await;
    f.native_account(&native).await;
    let bare = f.human("bare").await;
    let gone = f.member("gone").await;
    let oidc_bot = f.bot(&oidc, "a").await;
    let native_bot = f.bot(&native, "a").await;
    let bare_bot = f.bot(&bare, "a").await;
    let gone_bot = f.bot(&gone, "a").await;
    f.execute(
        "DELETE FROM users WHERE jid = ?",
        vec![Value::from(gone.as_str())],
    )
    .await;

    let mut rows = Vec::new();
    let mut after: Option<BareJid> = None;
    loop {
        let page = f
            .store
            .lifecycle_page(after.as_ref(), 3)
            .await
            .expect("page");
        let len = page.len();
        after = page.last().map(|row| row.bot.as_bare().clone());
        rows.extend(page);
        if len < 3 {
            break;
        }
    }

    let health = |bot: &BotJid| {
        let row = rows.iter().find(|row| &row.bot == bot).expect("bot listed");
        (row.owner_row_exists, row.owner_has_credential)
    };
    assert_eq!(rows.len(), 4);
    let mut order: Vec<_> = rows.iter().map(|row| row.bot.to_string()).collect();
    let listed = order.clone();
    order.sort();
    assert_eq!(listed, order, "rows come in JID order");
    assert_eq!(health(&oidc_bot), (true, true));
    assert_eq!(health(&native_bot), (true, true));
    assert_eq!(health(&bare_bot), (true, false));
    // The identity row cascades with the owner's users row.
    assert_eq!(health(&gone_bot), (false, false));
    assert!(rows.iter().all(|row| row.disabled_reason.is_none()));
}

/// The lifecycle query and the disable and enable transactions on
/// Postgres. Skipped unless `WADDLE_TEST_POSTGRES_URL` is set. Other tests
/// share the database, so this one never runs a whole sweep.
#[tokio::test]
async fn postgres_bot_lifecycle_disable_and_enable() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (postgres bot lifecycle)");
        return;
    };
    let db = Database::from_config(
        "bot-lifecycle-postgres",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url),
    )
    .await
    .expect("postgres database");
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("migrations");
    let f = fixture_on(db);
    let run = uuid::Uuid::new_v4().simple().to_string();
    let owner = f.member(&format!("owner{run}")).await;
    let missing = f.human(&format!("bare{run}")).await;
    let bot = f.bot(&owner, "standup").await;
    let orphan = f.bot(&missing, "standup").await;
    f.mint(&bot).await;

    let mut rows = Vec::new();
    let mut after: Option<BareJid> = None;
    loop {
        let page = f
            .store
            .lifecycle_page(after.as_ref(), 50)
            .await
            .expect("page");
        let len = page.len();
        after = page.last().map(|row| row.bot.as_bare().clone());
        rows.extend(
            page.into_iter()
                .filter(|row| row.bot == bot || row.bot == orphan),
        );
        if len < 50 {
            break;
        }
    }
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let expected = (true, row.bot == bot);
        assert_eq!((row.owner_row_exists, row.owner_has_credential), expected);
        assert_eq!(row.disabled_reason, None);
    }

    let report = f
        .store
        .disable(&bot, DisabledReason::OwnerMissing, Utc::now())
        .await
        .expect("disable");
    assert_eq!(
        report,
        DisableBotReport {
            disabled: true,
            tokens_revoked: 1,
        }
    );
    assert_eq!(f.disabled(&bot).await.1.as_deref(), Some("owner_missing"));
    assert_eq!(f.unrevoked_tokens(&bot).await, 0);
    assert!(f.store.enable(&bot, Utc::now()).await.expect("enable"));
    assert_eq!(f.disabled(&bot).await, (None, None));
    assert_eq!(f.unrevoked_tokens(&bot).await, 0);
}
