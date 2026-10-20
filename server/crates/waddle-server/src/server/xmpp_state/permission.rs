use kameo::actor::ActorRef;
use tracing::{debug, warn};
use waddle_xmpp::{Affiliation, XmppError};

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

/// Resolve a permission subject string to a bare JID.
///
/// The user principal is the bare JID itself, so a `user:` subject id is
/// already a bare JID — no database lookup is required. Userset subjects and
/// non-user subjects do not map to a JID.
pub(crate) fn resolve_subject_jid(subject: &str) -> Result<Option<jid::BareJid>, XmppError> {
    let subject = parse_subject(subject)?;
    if subject.subject_type != SubjectType::User || subject.relation.is_some() {
        return Ok(None);
    }

    let jid = subject
        .id
        .parse::<jid::BareJid>()
        .map_err(|e| XmppError::internal(format!("Failed to parse user JID: {}", e)))?;
    Ok(Some(jid))
}

pub(crate) async fn set_room_affiliation(
    permission_actor: &ActorRef<PermissionActor>,
    channel_id: &str,
    jid: &jid::BareJid,
    affiliation: Affiliation,
) -> Result<(), XmppError> {
    if jid.node().is_none() {
        return Err(XmppError::bad_request(Some("JID has no localpart".into())));
    }

    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(jid.to_string());
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
