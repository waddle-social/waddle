//! The identity profile at the account stores: canonical configured
//! domains, exact provisioning and lookup, OIDC name derivation, admin
//! command addresses, and the reconciliation report for accounts written
//! before the profile. In-memory SQLite, no server process.

use kameo::actor::{ActorRef, Spawn};
use waddle_server::admin::channels::{
    parse_group_dm_create_args, parse_kick_args, parse_set_affiliation_args,
};
use waddle_server::admin::spaces::parse_set_role_args;
use waddle_server::auth::identity_profile::{
    reconcile_identity_profile, require_canonical_domains, AccountFinding, AccountRecord,
};
use waddle_server::auth::{
    local_account_exists, username_to_localpart, AuthError, NativeUserStore, RegisterRequest,
};
use waddle_server::db::actor::{DbActor, DbExecute};
use waddle_server::db::{Database, MigrationRunner, Value};
use waddle_server::server::{seed_fixed_test_account, FixedTestAccountConfig};
use waddle_xmpp::identity::{AccountLocalpart, CanonicalDomain, IdentityError};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldType, FormType};

const DOMAIN: &str = "localhost";

async fn accounts_actor(name: &str) -> ActorRef<DbActor> {
    let db = Database::in_memory(name).await.expect("in-memory database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    DbActor::spawn(DbActor::new(db))
}

async fn execute(actor: &ActorRef<DbActor>, sql: &str, params: Vec<Value>) {
    actor
        .ask(DbExecute {
            sql: sql.to_owned(),
            params,
        })
        .await
        .expect("seed row");
}

/// An OIDC row as an older node wrote it: the stored JID, localpart and the
/// key that node computed.
async fn seed_oidc(actor: &ActorRef<DbActor>, jid: &str, localpart: &str, key: Option<&str>) {
    execute(
        actor,
        "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) \
         VALUES (?, ?, ?, ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        vec![
            jid.into(),
            localpart.into(),
            localpart.into(),
            key.map_or(Value::NullText, Value::from),
        ],
    )
    .await;
}

/// A native row as an older node wrote it, bypassing registration checks.
async fn seed_native(actor: &ActorRef<DbActor>, username: &str, domain: &str, key: Option<&str>) {
    execute(
        actor,
        "INSERT INTO native_users (username, domain, jid_key, password_hash, salt, stored_key, server_key) \
         VALUES (?, ?, ?, 'hash', 'c2FsdA==', x'00', x'00')",
        vec![
            username.into(),
            domain.into(),
            key.map_or(Value::NullText, Value::from),
        ],
    )
    .await;
}

/// The `auth_identities` row an identity provider sign-in writes for an
/// OIDC account it provisioned.
async fn seed_idp_identity(actor: &ActorRef<DbActor>, user_jid: &str, subject: &str) {
    execute(
        actor,
        "INSERT INTO auth_identities (id, user_jid, provider_id, issuer, subject, raw_claims_json, created_at, last_login_at) \
         VALUES (?, ?, 'idp', 'https://idp.example', ?, '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        vec![
            format!("identity-{subject}").into(),
            user_jid.into(),
            subject.into(),
        ],
    )
    .await;
}

async fn native_id(actor: &ActorRef<DbActor>, username: &str) -> i64 {
    let row = actor
        .ask(waddle_server::db::actor::DbQueryOne {
            sql: "SELECT id FROM native_users WHERE username = ?".to_owned(),
            params: vec![username.into()],
        })
        .await
        .expect("query")
        .expect("native row");
    match row.first() {
        Some(Value::Integer(id)) => *id,
        other => panic!("unexpected id {other:?}"),
    }
}

fn register(username: &str) -> RegisterRequest {
    RegisterRequest {
        username: username.to_owned(),
        domain: DOMAIN.to_owned(),
        password: "correct-horse-battery".to_owned(),
        email: None,
    }
}

#[test]
fn configured_domains_must_already_be_canonical() {
    let domain = require_canonical_domains("example.com", &["muc.example.com"]).expect("canonical");
    assert_eq!(domain.as_str(), "example.com");
    assert_eq!(
        require_canonical_domains("xn--bcher-kva.example", &[])
            .expect("A-label domain")
            .as_str(),
        "xn--bcher-kva.example"
    );
    for (account, service) in [
        ("Example.com", "muc.example.com"),
        ("example.com.", "muc.example.com"),
        ("bücher.example", "muc.example.com"),
        ("example.com", "MUC.example.com"),
        ("example.com", "muc.example.com."),
    ] {
        assert!(
            matches!(
                require_canonical_domains(account, &[service]),
                Err(IdentityError::NonCanonicalDomain)
            ),
            "{account} / {service}"
        );
    }
}

#[tokio::test]
async fn provisioning_accepts_only_the_account_profile() {
    let actor = accounts_actor("identity-profile-provisioning").await;
    let store = NativeUserStore::new(actor.clone());
    store
        .register(register("alice+work"))
        .await
        .expect("account profile name");
    for refused in ["Alice", "äda", "ａｌｉｃｅ", "\u{fb01}x", "a b", ""] {
        assert!(
            matches!(
                store.register(register(refused)).await,
                Err(AuthError::InvalidUsername(_))
            ),
            "{refused:?}"
        );
    }
    let mut foreign = register("bob");
    foreign.domain = "Localhost".to_owned();
    assert!(matches!(
        store.register(foreign).await,
        Err(AuthError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn login_and_directory_lookup_take_names_exactly() {
    let actor = accounts_actor("identity-profile-lookup").await;
    let store = NativeUserStore::new(actor.clone());
    store.register(register("ada")).await.expect("register ada");
    // A legacy native `Bob` keyed under `bob`, and a legacy OIDC `straße`
    // keyed under `strasse`, as nodes before the profile wrote them.
    seed_native(&actor, "Bob", DOMAIN, Some("bob@localhost")).await;
    seed_oidc(&actor, "straße@localhost", "straße", Some("strasse")).await;

    for (username, found) in [("ada", true), ("ADA", false), ("Ada", false)] {
        assert_eq!(
            store
                .get_scram_credentials(username, DOMAIN)
                .await
                .expect("lookup")
                .is_some(),
            found,
            "{username}"
        );
    }
    // The legacy account signs in only under the spelling its key records.
    assert!(store
        .get_scram_credentials("bob", DOMAIN)
        .await
        .expect("lookup")
        .is_some());
    assert!(store
        .get_scram_credentials("Bob", DOMAIN)
        .await
        .expect("lookup")
        .is_none());

    for (name, exists) in [
        ("ada", true),
        ("bob", true),
        ("strasse", true),
        ("Ada", false),
        ("BOB", false),
        ("straße", false),
        ("ａｄａ", false),
        ("ada\u{ad}", false),
    ] {
        assert_eq!(
            local_account_exists(&actor, name, DOMAIN)
                .await
                .expect("directory lookup"),
            exists,
            "{name:?}"
        );
    }
}

#[tokio::test]
async fn reconciliation_reports_accounts_outside_the_profile() {
    let actor = accounts_actor("identity-profile-reconciliation").await;
    let domain = CanonicalDomain::parse_exact(DOMAIN).expect("canonical domain");
    NativeUserStore::new(actor.clone())
        .register(register("clean"))
        .await
        .expect("register clean account");
    seed_oidc(&actor, "alice@localhost", "alice", Some("alice")).await;
    // Canonical but outside the ASCII account profile.
    seed_oidc(&actor, "josé@localhost", "josé", Some("josé")).await;
    // Spellings preparation would change.
    seed_oidc(&actor, "straße@localhost", "straße", Some("strasse")).await;
    seed_native(&actor, "Carol", DOMAIN, Some("carol@localhost")).await;
    // Unkeyed, with an OIDC row of the same name that no identity
    // provider provisioned (no `auth_identities` row): one account in both
    // stores, not a collision.
    seed_native(&actor, "alice", DOMAIN, None).await;
    // An identity provider provisioned an OIDC account under a native
    // account's exact name: two accounts, one address.
    seed_native(&actor, "bob", DOMAIN, Some("bob@localhost")).await;
    seed_oidc(&actor, "bob@localhost", "bob", Some("bob")).await;
    seed_idp_identity(&actor, "bob@localhost", "bob-subject").await;
    // Folding onto the native `Carol` across the stores.
    seed_oidc(&actor, "carol@localhost", "carol", Some("carol")).await;
    // Another deployment's domain.
    seed_native(
        &actor,
        "remote",
        "elsewhere.test",
        Some("remote@elsewhere.test"),
    )
    .await;
    // A recorded key that is not the stored name's key.
    seed_native(&actor, "dave", DOMAIN, Some("eve@localhost")).await;

    let report = reconcile_identity_profile(&actor, &domain)
        .await
        .expect("reconciliation");
    assert_eq!(report.accounts, 11);
    assert!(!report.is_clean());

    let native = |username: &'static str| {
        let actor = actor.clone();
        async move {
            AccountRecord::Native {
                id: native_id(&actor, username).await,
            }
        }
    };
    let oidc = |jid: &str| AccountRecord::Oidc {
        jid: jid.to_owned(),
    };
    let clean = native("clean").await;
    assert_eq!(report.findings(&clean), None);
    assert_eq!(
        report.findings(&oidc("josé@localhost")),
        Some(&[AccountFinding::OutsideAccountProfile][..])
    );
    assert_eq!(
        report.findings(&oidc("straße@localhost")),
        Some(&[AccountFinding::NonCanonicalSpelling][..])
    );
    assert_eq!(
        report.findings(&native("Carol").await),
        Some(
            &[
                AccountFinding::NonCanonicalSpelling,
                AccountFinding::FoldedCollision
            ][..]
        )
    );
    assert_eq!(
        report.findings(&oidc("carol@localhost")),
        Some(&[AccountFinding::FoldedCollision][..])
    );
    assert_eq!(
        report.findings(&native("alice").await),
        Some(&[AccountFinding::MissingKey][..])
    );
    assert_eq!(report.findings(&oidc("alice@localhost")), None);
    assert_eq!(
        report.findings(&native("bob").await),
        Some(&[AccountFinding::FoldedCollision][..])
    );
    assert_eq!(
        report.findings(&oidc("bob@localhost")),
        Some(&[AccountFinding::FoldedCollision][..])
    );
    assert_eq!(
        report.findings(&native("remote").await),
        Some(&[AccountFinding::ForeignDomain][..])
    );
    assert_eq!(
        report.findings(&native("dave").await),
        Some(&[AccountFinding::KeyMismatch][..])
    );
    assert_eq!(report.count(AccountFinding::FoldedCollision), 4);
    assert_eq!(report.outside.len(), 9);

    // The report is read only: running it again finds the same.
    assert_eq!(
        reconcile_identity_profile(&actor, &domain)
            .await
            .expect("second reconciliation"),
        report
    );
}

#[test]
fn oidc_names_are_transliterated_into_the_account_profile() {
    for (name, slug) in [
        ("Øyvind", Some("oyvind")),
        ("José", Some("jose")),
        ("Straße", Some("strasse")),
        ("Ærøskøbing", Some("aeroskobing")),
        ("Łukasz", Some("lukasz")),
        ("ＡＢＣ", Some("abc")),
        ("Alice+Dev", Some("alicedev")),
        ("李小龙", None),
        ("", None),
    ] {
        let derived = username_to_localpart(name);
        assert_eq!(derived.as_deref(), slug, "{name:?}");
        if let Some(derived) = derived {
            assert!(AccountLocalpart::parse(&derived).is_ok(), "{name:?}");
        }
    }
}

#[tokio::test]
async fn a_fixed_account_is_validated_before_it_is_reset() {
    assert_eq!(
        NativeUserStore::validate_new_account("admin", DOMAIN)
            .expect("account profile")
            .as_str(),
        "admin@localhost"
    );

    // Seeding deletes the configured name's account before registering it,
    // and the delete finds the row by its folded key. An invalid fixed
    // account that folds onto an existing one (`Admin`, or `admin` at
    // `Localhost`) must fail before that delete, leaving `admin` intact.
    let actor = accounts_actor("identity-profile-fixed-account").await;
    let store = NativeUserStore::new(actor.clone());
    store
        .register(register("admin"))
        .await
        .expect("register admin");
    let original = admin_credentials(&store)
        .await
        .expect("existing admin credentials");
    for (username, domain) in [("Admin", DOMAIN), ("admin", "Localhost"), ("äda", DOMAIN)] {
        assert!(
            NativeUserStore::validate_new_account(username, domain).is_err(),
            "{username}@{domain}"
        );
        let seeded = seed_fixed_test_account(
            &actor,
            &FixedTestAccountConfig {
                username: username.to_owned(),
                password: "replacement-password".to_owned(),
                domain: domain.to_owned(),
                email: None,
            },
        )
        .await;
        assert!(seeded.is_err(), "{username}@{domain}");
        assert_eq!(
            admin_credentials(&store).await.as_ref(),
            Some(&original),
            "{username}@{domain} reset the existing account"
        );
    }
    let rows = actor
        .ask(waddle_server::db::actor::DbQuery {
            sql: "SELECT username, domain FROM native_users".to_owned(),
            params: vec![],
        })
        .await
        .expect("native rows");
    assert_eq!(
        rows,
        vec![vec![Value::from("admin"), Value::from(DOMAIN)]],
        "no fixed account was written"
    );
}

/// The salt and stored key of `admin@localhost`, which a reset replaces.
async fn admin_credentials(store: &NativeUserStore) -> Option<(String, Vec<u8>)> {
    store
        .get_scram_credentials("admin", DOMAIN)
        .await
        .expect("credentials lookup")
        .map(|credentials| (credentials.salt_b64, credentials.stored_key))
}

fn form(fields: Vec<Field>) -> DataForm {
    fields
        .into_iter()
        .fold(DataForm::new(FormType::Submit), DataForm::add_field)
}

fn text(var: &str, value: &str) -> Field {
    Field::new(var, FieldType::TextSingle).with_value(value)
}

#[test]
fn admin_command_jids_are_taken_exactly() {
    // Fields submitted without a `type` reach the command parsers as text;
    // the parsers hold them to the identity profile themselves.
    let group_dm = |members: &[&str]| {
        let mut field = Field::new("member_jids", FieldType::TextSingle);
        for member in members {
            field = field.add_value(*member);
        }
        parse_group_dm_create_args(Some(&form(vec![text("name", "pair"), field])))
    };
    let created = group_dm(&["alice@localhost", " bob@localhost ", ""]).expect("canonical members");
    assert_eq!(created.member_jids.len(), 2);
    for members in [
        &["alice@localhost", "Bob@localhost"][..],
        &["alice@localhost", "stra\u{df}e@localhost"][..],
        &["alice@localhost", "bob@LOCALHOST"][..],
    ] {
        assert!(group_dm(members).is_err(), "{members:?}");
    }

    let affiliation = |member: &str| {
        parse_set_affiliation_args(Some(&form(vec![
            text("channel_jid", "room@muc.localhost"),
            text("member_jid", member),
            text("affiliation", "member"),
        ])))
    };
    assert!(affiliation("bob@localhost").is_ok());
    assert!(affiliation("Bob@localhost").is_err());

    let kick = |occupant: &str| {
        parse_kick_args(Some(&form(vec![
            text("channel_jid", "room@muc.localhost"),
            text("occupant_jid", occupant),
        ])))
    };
    assert!(kick("bob@localhost").is_ok());
    assert!(kick("\u{FF42}ob@localhost").is_err());

    let role = |member: &str| {
        parse_set_role_args(Some(&form(vec![
            text("space_jid", "space@spaces.localhost"),
            text("member_jid", member),
            text("role", "member"),
        ])))
    };
    assert!(role("bob@localhost").is_ok());
    assert!(role("BOB@localhost").is_err());
}
