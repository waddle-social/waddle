use kameo::actor::ActorRef;
use kameo::error::SendError;
use tracing::{debug, info, warn};

use crate::db::actor::{DbActor, DbQuery};
use crate::db::{row_value, ValueExt};
use crate::permissions::{
    CheckPermission, Object, ObjectType, Permission, PermissionActor, PermissionError, Relation,
    Subject, Tuple, WriteTuple,
};

pub const DEPLOYMENT_SERVER_ID: &str = "deployment";
const OWNER_ENV: &str = "WADDLE_SERVER_OWNER_LOCALPARTS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapMembershipConfig {
    pub server_id: String,
    owner_localparts: Vec<String>,
}

impl BootstrapMembershipConfig {
    pub fn from_env() -> Self {
        Self {
            server_id: DEPLOYMENT_SERVER_ID.to_string(),
            owner_localparts: parse_owner_localparts(
                std::env::var(OWNER_ENV).unwrap_or_default().as_str(),
            ),
        }
    }

    #[cfg(test)]
    pub fn new(owner_localparts: Vec<String>) -> Self {
        Self {
            server_id: DEPLOYMENT_SERVER_ID.to_string(),
            owner_localparts: owner_localparts
                .into_iter()
                .filter_map(|value| normalize_localpart(&value))
                .collect(),
        }
    }

    pub fn is_owner(&self, localpart: &str) -> bool {
        normalize_localpart(localpart).is_some_and(|localpart| {
            self.owner_localparts
                .iter()
                .any(|owner| owner == &localpart)
        })
    }
}

pub fn parse_owner_localparts(value: &str) -> Vec<String> {
    let mut localparts = Vec::new();
    for part in value.split([',', ' ', '\n', '\t']) {
        let Some(localpart) = normalize_localpart(part) else {
            continue;
        };
        if !localparts.iter().any(|existing| existing == &localpart) {
            localparts.push(localpart);
        }
    }
    localparts
}

fn normalize_localpart(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('@');
    if trimmed.is_empty() {
        return None;
    }
    let localpart = trimmed
        .split('@')
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    if localpart.is_empty() {
        None
    } else {
        Some(localpart)
    }
}

pub async fn provision_user_membership(
    permission_actor: &ActorRef<PermissionActor>,
    config: &BootstrapMembershipConfig,
    user_id: &str,
    xmpp_localpart: &str,
) -> Result<(), String> {
    let subject = Subject::user(user_id);
    if config.is_owner(xmpp_localpart) {
        let object = Object::new(ObjectType::Server, config.server_id.as_str());
        let tuple = Tuple::new(object, Relation::new("owner"), subject);
        write_tuple_if_absent(permission_actor, tuple).await?;

        debug!(
            user_id = %user_id,
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
        user_id = %user_id,
        xmpp_localpart = %xmpp_localpart,
        relation = "member",
        "Provisioned bootstrap membership"
    );
    Ok(())
}

pub async fn reconcile_user_membership(
    permission_actor: &ActorRef<PermissionActor>,
    config: &BootstrapMembershipConfig,
    user_id: &str,
    xmpp_localpart: &str,
) -> Result<(), String> {
    if config.is_owner(xmpp_localpart) {
        return provision_user_membership(permission_actor, config, user_id, xmpp_localpart).await;
    }

    let subject = Subject::user(user_id);
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

    provision_user_membership(permission_actor, config, user_id, xmpp_localpart).await
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
            sql: "SELECT id, xmpp_localpart FROM users".to_string(),
            params: vec![],
        })
        .await
        .map_err(|err| format!("failed listing users for membership bootstrap: {err}"))?;

    for row in users {
        let user_id = row_value(&row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|err| format!("failed decoding user id: {err}"))?;
        let localpart = row_value(&row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|err| format!("failed decoding user localpart: {err}"))?;
        reconcile_user_membership(permission_actor, config, &user_id, &localpart).await?;
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
        let user_id = format!("{username}@{domain}");
        reconcile_user_membership(permission_actor, config, &user_id, &username).await?;
    }

    info!("Reconciled bootstrap memberships for existing accounts");
    Ok(())
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
    use std::sync::Arc;

    #[test]
    fn parses_owner_localparts_from_env_style_list() {
        assert_eq!(
            parse_owner_localparts(" rawkode,icepuma\nrandax rawkode "),
            vec!["rawkode", "icepuma", "randax"]
        );
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
        let actor = kameo::spawn(PermissionActor::new_for_tests(db));
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
        let actor = kameo::spawn(PermissionActor::new_for_tests(db));
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
