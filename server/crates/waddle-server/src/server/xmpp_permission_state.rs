use kameo::actor::ActorRef;
use tracing::{debug, warn};
use waddle_xmpp::{Affiliation, XmppError};

use crate::auth::localpart_to_jid;
use crate::db::actor::{DbActor, DbQueryOne, RowValues};
use crate::db::{ValueExt, row_value};
use crate::permissions::{
    CheckPermission, DeleteTuple, ListRelations, ListSubjects, Object, ObjectType, Permission,
    PermissionActor, Relation, Subject, SubjectType, Tuple, WriteTuple,
};

pub(crate) fn parse_resource(resource: &str) -> Result<Object, XmppError> {
    Object::parse(resource)
        .map_err(|e| XmppError::internal(format!("Invalid resource format '{}': {}", resource, e)))
}

pub(crate) fn parse_subject(subject: &str) -> Result<Subject, XmppError> {
    Subject::parse(subject)
        .map_err(|e| XmppError::internal(format!("Invalid subject format '{}': {}", subject, e)))
}

pub(crate) async fn check_permission(
    permission_actor: &ActorRef<PermissionActor>,
    resource: &str,
    action: &str,
    subject: &str,
) -> Result<bool, XmppError> {
    debug!(
        resource = resource,
        action = action,
        subject = subject,
        "Checking XMPP permission"
    );

    let object = parse_resource(resource)?;
    let subject = parse_subject(subject)?;

    let response = permission_actor
        .ask(CheckPermission {
            subject,
            permission: Permission::Custom(action.to_string()),
            object,
        })
        .await
        .map_err(|e| {
            warn!(
                resource = resource,
                action = action,
                error = %e,
                "Permission check failed"
            );
            XmppError::internal(format!("Permission check failed: {}", e))
        })?;

    debug!(
        resource = resource,
        action = action,
        allowed = response.allowed,
        "Permission check result"
    );

    Ok(response.allowed)
}

pub(crate) async fn list_relations(
    permission_actor: &ActorRef<PermissionActor>,
    resource: &str,
    subject: &str,
) -> Result<Vec<String>, XmppError> {
    debug!(
        resource = resource,
        subject = subject,
        "Listing relations for subject on resource"
    );

    let object = parse_resource(resource)?;
    let subject = parse_subject(subject)?;

    let relations = permission_actor
        .ask(ListRelations { subject, object })
        .await
        .map_err(|e| {
            warn!(
                resource = resource,
                error = %e,
                "Failed to list relations"
            );
            XmppError::internal(format!("Failed to list relations: {}", e))
        })?;

    debug!(
        resource = resource,
        relations = ?relations,
        "Listed relations"
    );

    Ok(relations
        .into_iter()
        .map(|relation| relation.name)
        .collect())
}

pub(crate) async fn list_subjects(
    permission_actor: &ActorRef<PermissionActor>,
    resource: &str,
    relation: &str,
) -> Result<Vec<String>, XmppError> {
    debug!(
        resource = resource,
        relation = relation,
        "Listing subjects with relation on resource"
    );

    let object = parse_resource(resource)?;

    let subjects = permission_actor
        .ask(ListSubjects {
            object,
            relation: Relation::new(relation),
        })
        .await
        .map_err(|e| {
            warn!(
                resource = resource,
                relation = relation,
                error = %e,
                "Failed to list subjects"
            );
            XmppError::internal(format!("Failed to list subjects: {}", e))
        })?;

    let subject_strings: Vec<String> = subjects.iter().map(|s| s.to_string()).collect();

    debug!(
        resource = resource,
        relation = relation,
        count = subject_strings.len(),
        "Listed subjects"
    );

    Ok(subject_strings)
}

pub(crate) async fn resolve_subject_jid(
    global_db_actor: &ActorRef<DbActor>,
    domain: &str,
    subject: &str,
) -> Result<Option<jid::BareJid>, XmppError> {
    let subject = parse_subject(subject)?;
    if subject.subject_type != SubjectType::User || subject.relation.is_some() {
        return Ok(None);
    }

    let row = global_db_actor
        .ask(DbQueryOne {
            sql: "SELECT xmpp_localpart FROM users WHERE id = ? LIMIT 1".to_string(),
            params: vec![subject.id.as_str().into()],
        })
        .await
        .map_err(|e| XmppError::internal(format!("Failed to resolve user JID: {}", e)))?;

    let Some(row) = row else {
        return Ok(None);
    };
    let localpart = db_string(&row, 0, "xmpp_localpart")
        .map_err(|e| XmppError::internal(format!("Failed to decode user JID: {}", e)))?;
    let jid = localpart_to_jid(&localpart, domain)
        .map_err(|e| XmppError::internal(format!("Failed to build user JID: {}", e)))?
        .parse()
        .map_err(|e| XmppError::internal(format!("Failed to parse user JID: {}", e)))?;
    Ok(Some(jid))
}

pub(crate) async fn set_room_affiliation(
    global_db_actor: &ActorRef<DbActor>,
    permission_actor: &ActorRef<PermissionActor>,
    channel_id: &str,
    jid: &jid::BareJid,
    affiliation: Affiliation,
) -> Result<(), XmppError> {
    let Some(localpart) = jid.node() else {
        return Err(XmppError::bad_request(Some("JID has no localpart".into())));
    };
    let row = global_db_actor
        .ask(DbQueryOne {
            sql: "SELECT id FROM users WHERE xmpp_localpart = ? LIMIT 1".to_string(),
            params: vec![localpart.as_str().into()],
        })
        .await
        .map_err(|e| XmppError::internal(format!("Failed to resolve user: {}", e)))?;
    let Some(row) = row else {
        return Err(XmppError::item_not_found(Some(format!(
            "User {} not found",
            jid
        ))));
    };
    let user_id = db_string(&row, 0, "id")
        .map_err(|e| XmppError::internal(format!("Failed to decode user id: {}", e)))?;

    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(&user_id);
    for relation in ["owner", "admin", "member", "outcast"] {
        let tuple = Tuple::new(object.clone(), Relation::new(relation), subject.clone());
        permission_actor
            .ask(DeleteTuple { tuple })
            .await
            .map_err(|e| {
                XmppError::internal(format!("Failed to clear MUC affiliation tuple: {}", e))
            })?;
    }

    let relation = match affiliation {
        Affiliation::Owner => Some("owner"),
        Affiliation::Admin => Some("admin"),
        Affiliation::Member => Some("member"),
        Affiliation::Outcast => Some("outcast"),
        Affiliation::None => None,
    };

    if let Some(relation) = relation {
        let tuple = Tuple::new(object, Relation::new(relation), subject);
        permission_actor
            .ask(WriteTuple { tuple })
            .await
            .map_err(|e| {
                XmppError::internal(format!("Failed to write MUC affiliation tuple: {}", e))
            })?;
    }

    Ok(())
}

fn db_string(row: &RowValues, index: usize, name: &str) -> Result<String, String> {
    row_value(row, index)
        .and_then(ValueExt::as_string)
        .map_err(|e| format!("Failed to get {name}: {e}"))
}
