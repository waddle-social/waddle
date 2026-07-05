//! Production [`DurableMembershipSource`] backed by the permission actor.
//!
//! Bridges the deployment's Zanzibar-style permission tuples to the
//! `waddle-xmpp` room registry so every freshly spawned `RoomActor`
//! hydrates its durable inbox recipient set at spawn (#1135) —
//! offline members keep receiving inbox rows / notification
//! candidates across deploys and actor respawns.

use std::collections::HashSet;
use std::time::Duration;

use jid::BareJid;
use kameo::actor::ActorRef;
use tracing::warn;
use waddle_xmpp::muc::affiliation::{DurableMembershipFuture, DurableMembershipSource};
use waddle_xmpp::XmppError;

use crate::permissions::{
    ListSubjects, Object, ObjectType, PermissionActor, Relation, Subject, SubjectType,
};

/// Membership-granting relations, mirroring
/// [`waddle_xmpp::muc::affiliation::AppStateAffiliationResolver`]:
/// owner/admin/member at either the channel or the space level all
/// resolve to `Affiliation::Member`+.
const MEMBER_RELATIONS: [&str; 3] = ["owner", "admin", "member"];

/// Upper bound on each `ListSubjects` ask (S6). Hydration is the first
/// message in a freshly spawned `RoomActor`'s mailbox, so an unbounded
/// ask against a hung (not dead) [`PermissionActor`] would wedge the
/// room — and the dormancy sweep behind it — forever. A timeout maps to
/// the existing fail-open `Err` path. Magnitude matches the registry's
/// `ROOM_REGISTRY_REPLY_TIMEOUT` (5s).
const MEMBERSHIP_LOOKUP_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Durable membership source reading `permission_tuples` through the
/// shared [`PermissionActor`].
pub struct PermissionDurableMembershipSource {
    permission_actor: ActorRef<PermissionActor>,
}

impl PermissionDurableMembershipSource {
    pub fn new(permission_actor: ActorRef<PermissionActor>) -> Self {
        Self { permission_actor }
    }

    async fn subjects(&self, object: Object, relation: &str) -> Result<Vec<Subject>, XmppError> {
        self.permission_actor
            .ask(ListSubjects {
                object,
                relation: Relation::new(relation),
            })
            .reply_timeout(MEMBERSHIP_LOOKUP_REPLY_TIMEOUT)
            .await
            .map_err(|error| {
                XmppError::internal(format!("durable membership lookup failed: {error}"))
            })
    }
}

/// Direct user subjects carry the bare JID as their id (see
/// `xmpp_permission_state::resolve_subject_jid`). Userset and
/// non-user subjects do not map to a JID; a malformed user id is
/// logged and skipped rather than failing the whole hydration.
fn subject_bare_jid(subject: &Subject) -> Option<BareJid> {
    if subject.subject_type != SubjectType::User || subject.relation.is_some() {
        return None;
    }
    match subject.id.parse::<BareJid>() {
        Ok(jid) => Some(jid),
        Err(error) => {
            warn!(
                subject = %subject.id,
                %error,
                "skipping durable member with unparseable bare JID"
            );
            None
        }
    }
}

impl DurableMembershipSource for PermissionDurableMembershipSource {
    fn list_durable_member_jids(
        &self,
        waddle_id: &str,
        channel_id: &str,
    ) -> DurableMembershipFuture<'_> {
        let waddle_id = waddle_id.to_string();
        let channel_id = channel_id.to_string();
        Box::pin(async move {
            // Mirrors the join-path affiliation derivation
            // (`AppStateAffiliationResolver`): channel- AND
            // space-level owner/admin/member all grant Member+;
            // a channel-level outcast relation excludes the user.
            let scopes = [
                Object::new(ObjectType::Channel, channel_id.clone()),
                Object::new(ObjectType::Space, waddle_id),
            ];
            let mut members = Vec::new();
            for scope in &scopes {
                for relation in MEMBER_RELATIONS {
                    for subject in self.subjects(scope.clone(), relation).await? {
                        if let Some(jid) = subject_bare_jid(&subject) {
                            members.push(jid);
                        }
                    }
                }
            }
            let outcasts: HashSet<BareJid> = self
                .subjects(Object::new(ObjectType::Channel, channel_id), "outcast")
                .await?
                .iter()
                .filter_map(subject_bare_jid)
                .collect();
            members.retain(|jid| !outcasts.contains(jid));
            members.sort();
            members.dedup();
            Ok(members)
        })
    }
}

#[cfg(test)]
#[path = "durable_membership_tests.rs"]
mod tests;
