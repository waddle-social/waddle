use kameo::actor::ActorRef;
use kameo::error::SendError;
use tracing::{debug, info, warn};
use waddle_xmpp::identity::{parse_localpart, IdentityError};

use crate::db::actor::{DbActor, DbQuery};
use crate::db::{row_value, ValueExt};
use crate::permissions::{
    AuthzIdError, AuthzUserJid, CheckPermission, Object, ObjectType, Permission, PermissionActor,
    PermissionError, Relation, Subject, Tuple, WriteTuple,
};

pub const DEPLOYMENT_SERVER_ID: &str = "deployment";
const OWNER_ENV: &str = "WADDLE_SERVER_OWNER_LOCALPARTS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapMembershipConfig {
    pub server_id: String,
    owner_localparts: Vec<String>,
    /// The account domain the deployment serves. Deployment ownership is a
    /// privileged grant, so it is provisioned only for an account of this
    /// domain (ADR-0020 D8); bootstrap is given no trusted home.
    account_domain: Option<String>,
}

impl BootstrapMembershipConfig {
    /// Load `WADDLE_SERVER_OWNER_LOCALPARTS` at startup. Every entry must be
    /// a localpart already in the identity profile's canonical form
    /// ([`parse_localpart`]); any other entry fails configuration load
    /// instead of being folded onto another account.
    pub fn try_from_env() -> Result<Self, OwnerLocalpartError> {
        Ok(Self {
            server_id: DEPLOYMENT_SERVER_ID.to_string(),
            owner_localparts: parse_owner_localparts(
                std::env::var(OWNER_ENV).unwrap_or_default().as_str(),
            )?,
            account_domain: None,
        })
    }

    /// The same configuration after startup has validated it with
    /// [`BootstrapMembershipConfig::try_from_env`]. A list with an entry
    /// outside the profile cannot reach a running server; were one present,
    /// it would name no owner at all rather than fold an entry onto an
    /// account.
    pub fn from_env() -> Self {
        Self::try_from_env().unwrap_or_else(|error| {
            warn!(%error, "ignoring server-owner configuration outside the identity profile");
            Self {
                server_id: DEPLOYMENT_SERVER_ID.to_string(),
                owner_localparts: Vec::new(),
                account_domain: None,
            }
        })
    }

    /// The same configuration for a deployment whose accounts live at
    /// `account_domain`. Every path that provisions membership passes it.
    pub fn serving(mut self, account_domain: &str) -> Self {
        self.account_domain = Some(account_domain.to_owned());
        self
    }

    #[cfg(test)]
    pub fn new(owner_localparts: Vec<String>) -> Self {
        Self {
            server_id: DEPLOYMENT_SERVER_ID.to_string(),
            owner_localparts,
            account_domain: None,
        }
    }

    /// Whether the account with exactly this stored localpart is a
    /// configured owner. The localpart is compared exactly: an account whose
    /// name differs from an entry only in case is not an owner.
    pub fn is_owner(&self, localpart: &str) -> bool {
        self.owner_localparts.iter().any(|owner| owner == localpart)
    }

    /// Whether `user_jid` may be provisioned as a deployment owner: its
    /// stored localpart is a configured owner, the JID is that account's
    /// bare JID, and its domain is the served account domain when one was
    /// given. Any other subject, a remote account in particular, gets no
    /// privileged grant from bootstrap.
    fn owner_subject(&self, user_jid: &str, localpart: &str) -> bool {
        if !self.is_owner(localpart) {
            return false;
        }
        let Ok(jid) = AuthzUserJid::parse_exact(user_jid) else {
            return false;
        };
        let jid = jid.jid();
        jid.node().is_some_and(|node| node.as_str() == localpart)
            && self
                .account_domain
                .as_deref()
                .is_none_or(|domain| jid.domain().as_str() == domain)
    }

    /// All server-owner localparts, canonical and deduplicated.
    pub fn owner_localparts(&self) -> &[String] {
        &self.owner_localparts
    }
}

/// A `WADDLE_SERVER_OWNER_LOCALPARTS` entry outside the identity profile.
/// The entry is named, since it is operator configuration, not account data
/// read from a store.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "WADDLE_SERVER_OWNER_LOCALPARTS entry {entry:?} is not a localpart in canonical form \
     (write the exact account name, without a domain): {reason}"
)]
pub struct OwnerLocalpartError {
    pub entry: String,
    pub reason: IdentityError,
}

/// Parse an owner list separated by commas, spaces, tabs or line breaks (LF
/// or CRLF). Each entry must be a canonical localpart exactly as written;
/// duplicates are removed.
pub fn parse_owner_localparts(value: &str) -> Result<Vec<String>, OwnerLocalpartError> {
    let mut localparts: Vec<String> = Vec::new();
    for entry in owner_entries(value) {
        parse_localpart(entry).map_err(|reason| OwnerLocalpartError {
            entry: entry.to_owned(),
            reason,
        })?;
        if !localparts.iter().any(|existing| existing == entry) {
            localparts.push(entry.to_owned());
        }
    }
    Ok(localparts)
}

fn owner_entries(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([',', ' ', '\n', '\r', '\t'])
        .filter(|entry| !entry.is_empty())
}

pub async fn provision_user_membership(
    permission_actor: &ActorRef<PermissionActor>,
    config: &BootstrapMembershipConfig,
    user_jid: &str,
    xmpp_localpart: &str,
) -> Result<(), String> {
    let subject = Subject::user(user_jid);
    if config.is_owner(xmpp_localpart) && !config.owner_subject(user_jid, xmpp_localpart) {
        warn!(
            "Refused to provision deployment ownership for an account outside the served \
             account domain"
        );
    }
    if config.owner_subject(user_jid, xmpp_localpart) {
        let object = Object::new(ObjectType::Server, config.server_id.as_str());
        let tuple = Tuple::new(object, Relation::new("owner"), subject);
        write_tuple_if_absent(permission_actor, tuple).await?;

        debug!(
            user_jid = %user_jid,
            xmpp_localpart = %xmpp_localpart,
            relation = "owner",
            "Provisioned deployment owner membership"
        );
        return Ok(());
    }

    let object = Object::new(ObjectType::Server, config.server_id.as_str());
    let tuple = Tuple::new(object, Relation::new("member"), subject);
    write_tuple_if_absent(permission_actor, tuple).await?;

    debug!(
        user_jid = %user_jid,
        xmpp_localpart = %xmpp_localpart,
        relation = "member",
        "Provisioned bootstrap membership"
    );
    Ok(())
}

pub async fn reconcile_user_membership(
    permission_actor: &ActorRef<PermissionActor>,
    config: &BootstrapMembershipConfig,
    user_jid: &str,
    xmpp_localpart: &str,
) -> Result<(), String> {
    if config.owner_subject(user_jid, xmpp_localpart) {
        return provision_user_membership(permission_actor, config, user_jid, xmpp_localpart).await;
    }

    let subject = Subject::user(user_jid);
    let object = Object::new(ObjectType::Server, config.server_id.as_str());
    for permission in [Permission::Owner, Permission::Admin, Permission::Member] {
        let response = permission_actor
            .ask(CheckPermission {
                subject: subject.clone(),
                permission,
                object: object.clone(),
            })
            .await
            .map_err(|err| format!("failed checking membership: {err}"))?;
        if response.allowed {
            return Ok(());
        }
    }

    provision_user_membership(permission_actor, config, user_jid, xmpp_localpart).await
}

async fn write_tuple_if_absent(
    permission_actor: &ActorRef<PermissionActor>,
    tuple: Tuple,
) -> Result<(), String> {
    match permission_actor.ask(WriteTuple { tuple }).await {
        Ok(()) => Ok(()),
        Err(SendError::ActorNotRunning(_)) => {
            Err("permission actor is not running while writing membership tuple".to_string())
        }
        Err(SendError::HandlerError(PermissionError::TupleAlreadyExists)) => Ok(()),
        Err(SendError::HandlerError(err)) => Err(format!("failed writing membership tuple: {err}")),
        Err(SendError::Timeout(_)) => {
            Err("timed out writing membership tuple through permission actor".to_string())
        }
        Err(err) => Err(format!(
            "permission actor failed writing membership tuple: {err}"
        )),
    }
}

pub async fn reconcile_existing_accounts(
    db_actor: &ActorRef<DbActor>,
    permission_actor: &ActorRef<PermissionActor>,
    config: &BootstrapMembershipConfig,
) -> Result<(), String> {
    let users = db_actor
        .ask(DbQuery {
            sql: "SELECT jid, xmpp_localpart FROM users".to_string(),
            params: vec![],
        })
        .await
        .map_err(|err| format!("failed listing users for membership bootstrap: {err}"))?;

    let mut skipped = 0_u64;
    for row in users {
        let user_jid = row_value(&row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|err| format!("failed decoding user jid: {err}"))?;
        let localpart = row_value(&row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|err| format!("failed decoding user localpart: {err}"))?;
        let subject = match AuthzUserJid::parse_exact(&user_jid) {
            Ok(subject) => subject,
            Err(error) => {
                skipped += 1;
                warn_unrepresentable_account("users", error);
                continue;
            }
        };
        reconcile_user_membership(permission_actor, config, subject.jid().as_str(), &localpart)
            .await?;
    }

    let native_users = db_actor
        .ask(DbQuery {
            sql: "SELECT username, domain FROM native_users".to_string(),
            params: vec![],
        })
        .await
        .map_err(|err| format!("failed listing native users for membership bootstrap: {err}"))?;

    for row in native_users {
        let username = row_value(&row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|err| format!("failed decoding native username: {err}"))?;
        let domain = row_value(&row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|err| format!("failed decoding native domain: {err}"))?;
        let subject = match AuthzUserJid::from_account_parts(&username, &domain) {
            Ok(subject) => subject,
            Err(error) => {
                skipped += 1;
                warn_unrepresentable_account("native_users", error);
                continue;
            }
        };
        reconcile_user_membership(permission_actor, config, subject.jid().as_str(), &username)
            .await?;
    }

    info!(
        skipped_accounts = skipped,
        "Reconciled bootstrap memberships for existing accounts"
    );
    Ok(())
}

/// An account whose address cannot be an authorization subject (for example
/// a stored username that the JID parser would rewrite) gets no bootstrap
/// membership. It is skipped so that the remaining accounts are still
/// reconciled; the log names the table and the reason, not the account.
fn warn_unrepresentable_account(table: &'static str, error: AuthzIdError) {
    warn!(
        table,
        reason = %error,
        "Skipping bootstrap membership for an account whose address is not a valid authorization subject"
    );
}

pub async fn reconcile_existing_accounts_or_warn(
    db_actor: &ActorRef<DbActor>,
    permission_actor: &ActorRef<PermissionActor>,
    config: &BootstrapMembershipConfig,
) {
    if let Err(error) = reconcile_existing_accounts(db_actor, permission_actor, config).await {
        warn!(error = %error, "Failed to reconcile bootstrap memberships");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, MigrationRunner};
    use crate::permissions::{CheckPermission, Permission};
    use kameo::actor::Spawn;
    use std::sync::Arc;

    #[test]
    fn parses_owner_localparts_from_env_style_list() {
        assert_eq!(
            parse_owner_localparts(" rawkode,icepuma\nrandax rawkode "),
            Ok(vec![
                "rawkode".to_owned(),
                "icepuma".to_owned(),
                "randax".to_owned()
            ])
        );
        // A list written with CRLF line endings, as a Windows-edited env
        // file or secret produces.
        assert_eq!(
            parse_owner_localparts("rawkode\r\nicepuma\r\n"),
            Ok(vec!["rawkode".to_owned(), "icepuma".to_owned()])
        );
        for refused in [
            "Rawkode",
            "rawkode@waddle.social",
            "@rawkode",
            "stra\u{df}e",
        ] {
            assert!(parse_owner_localparts(refused).is_err(), "{refused}");
        }
    }

    #[tokio::test]
    async fn provisions_server_owner_and_default_member_through_permission_actor() {
        let db = Arc::new(
            Database::in_memory("bootstrap-membership")
                .await
                .expect("database"),
        );
        MigrationRunner::global()
            .run(&db)
            .await
            .expect("migrations");
        let actor = PermissionActor::spawn(PermissionActor::new_for_tests(db));
        let config = BootstrapMembershipConfig::new(vec!["rawkode".to_string()]);

        provision_user_membership(&actor, &config, "user-owner", "rawkode")
            .await
            .expect("owner");
        provision_user_membership(&actor, &config, "user-member", "alice")
            .await
            .expect("member");

        let owner = actor
            .ask(CheckPermission {
                subject: Subject::user("user-owner"),
                permission: Permission::Owner,
                object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            })
            .await
            .expect("owner check");
        let member = actor
            .ask(CheckPermission {
                subject: Subject::user("user-member"),
                permission: Permission::Member,
                object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            })
            .await
            .expect("member check");
        assert!(owner.allowed);
        assert!(member.allowed);
    }

    #[tokio::test]
    async fn reconciliation_does_not_demote_existing_non_owner_roles() {
        let db = Arc::new(
            Database::in_memory("bootstrap-membership-existing")
                .await
                .expect("database"),
        );
        MigrationRunner::global()
            .run(&db)
            .await
            .expect("migrations");
        let actor = PermissionActor::spawn(PermissionActor::new_for_tests(db));
        let config = BootstrapMembershipConfig::new(vec!["rawkode".to_string()]);

        actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Space, "space"),
                    Relation::new("admin"),
                    Subject::user("user-admin"),
                ),
            })
            .await
            .expect("write admin");

        reconcile_user_membership(&actor, &config, "user-admin", "alice")
            .await
            .expect("reconcile");

        let admin = actor
            .ask(CheckPermission {
                subject: Subject::user("user-admin"),
                permission: Permission::Admin,
                object: Object::new(ObjectType::Space, "space"),
            })
            .await
            .expect("admin check");

        assert!(admin.allowed);
    }
}
