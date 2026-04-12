//! Kameo actor wrapping the permission service.
//!
//! The `PermissionActor` owns a `PermissionService` and processes all
//! permission operations as actor messages. This centralises permission
//! checking into a single actor with the LRU cache owned internally.
//! This is Phase 4 of the actor-model migration described in issue #42.

use kameo::message::Context;
use kameo::Actor;

use super::tuple::{Object, Subject, Tuple};
use super::PermissionService;

/// Actor that owns a [`PermissionService`] and handles permission operations
/// sequentially via message passing.
#[derive(Actor)]
pub struct PermissionActor {
    service: PermissionService,
}

impl PermissionActor {
    /// Create a new `PermissionActor` wrapping the given service.
    pub fn new(service: PermissionService) -> Self {
        Self { service }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Check whether a subject has a permission on an object.
///
/// The subject, permission, and object are provided as strings and parsed
/// internally (Zanzibar tuple format: `type:id` for subject/object).
pub struct CheckPermission {
    pub subject: String,
    pub permission: String,
    pub object: String,
}

impl kameo::message::Message<CheckPermission> for PermissionActor {
    type Reply = Result<bool, String>;

    async fn handle(
        &mut self,
        msg: CheckPermission,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let subject = Subject::parse(&msg.subject).map_err(|e| e.to_string())?;
        let object = Object::parse(&msg.object).map_err(|e| e.to_string())?;

        let response = self
            .service
            .check(&subject, &msg.permission, &object)
            .await
            .map_err(|e| e.to_string())?;

        Ok(response.allowed)
    }
}

/// Write a new permission tuple.
///
/// The tuple is provided in Zanzibar string format:
/// `object#relation@subject` (e.g. `waddle:test#owner@user:alice`).
pub struct WriteTuple {
    pub tuple_str: String,
}

impl kameo::message::Message<WriteTuple> for PermissionActor {
    type Reply = Result<(), String>;

    async fn handle(
        &mut self,
        msg: WriteTuple,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let tuple = Tuple::parse(&msg.tuple_str).map_err(|e| e.to_string())?;
        self.service
            .write_tuple(tuple)
            .await
            .map_err(|e| e.to_string())?;
        // Invalidate the permission cache after a write to ensure
        // subsequent checks reflect the new state.
        self.service.checker.clear_cache().await;
        Ok(())
    }
}

/// Delete an existing permission tuple.
///
/// The tuple is provided in Zanzibar string format:
/// `object#relation@subject`.
pub struct DeleteTuple {
    pub tuple_str: String,
}

impl kameo::message::Message<DeleteTuple> for PermissionActor {
    type Reply = Result<(), String>;

    async fn handle(
        &mut self,
        msg: DeleteTuple,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let tuple = Tuple::parse(&msg.tuple_str).map_err(|e| e.to_string())?;
        self.service
            .delete_tuple(&tuple)
            .await
            .map_err(|e| e.to_string())?;
        // Invalidate the permission cache after a delete to ensure
        // subsequent checks reflect the removal.
        self.service.checker.clear_cache().await;
        Ok(())
    }
}

/// List all relations a subject has on an object.
///
/// Both subject and object are in `type:id` format.
pub struct ListRelations {
    pub subject: String,
    pub object: String,
}

impl kameo::message::Message<ListRelations> for PermissionActor {
    type Reply = Result<Vec<String>, String>;

    async fn handle(
        &mut self,
        msg: ListRelations,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let subject = Subject::parse(&msg.subject).map_err(|e| e.to_string())?;
        let object = Object::parse(&msg.object).map_err(|e| e.to_string())?;

        self.service
            .list_relations(&subject, &object)
            .await
            .map_err(|e| e.to_string())
    }
}

/// List all subjects that hold a specific relation on an object.
///
/// The object is in `type:id` format and the relation is a plain string.
/// Returns subject strings in their display format (e.g. `user:alice`).
pub struct ListSubjects {
    pub object: String,
    pub relation: String,
}

impl kameo::message::Message<ListSubjects> for PermissionActor {
    type Reply = Result<Vec<String>, String>;

    async fn handle(
        &mut self,
        msg: ListSubjects,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let object = Object::parse(&msg.object).map_err(|e| e.to_string())?;

        let subjects = self
            .service
            .list_subjects(&object, &msg.relation)
            .await
            .map_err(|e| e.to_string())?;

        Ok(subjects.into_iter().map(|s| s.to_string()).collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kameo::actor::ActorRef;

    use super::*;
    use crate::db::{Database, MigrationRunner};

    async fn spawn_test_actor() -> ActorRef<PermissionActor> {
        let db = Database::in_memory("test-permission-actor")
            .await
            .expect("db");
        let db = Arc::new(db);

        let runner = MigrationRunner::global();
        runner.run(&db).await.expect("migrations");

        let service = PermissionService::new(db);
        kameo::spawn(PermissionActor::new(service))
    }

    #[tokio::test]
    async fn test_write_and_check_permission() {
        let actor = spawn_test_actor().await;

        // Write a tuple: alice is owner of waddle:test
        actor
            .ask(WriteTuple {
                tuple_str: "waddle:test#owner@user:user-alice".to_string(),
            })
            .await
            .expect("write should succeed");

        // Check: alice has owner permission
        let allowed: bool = actor
            .ask(CheckPermission {
                subject: "user:user-alice".to_string(),
                permission: "owner".to_string(),
                object: "waddle:test".to_string(),
            })
            .await
            .expect("check");
        assert!(allowed);

        // Check: bob does NOT have owner permission
        let allowed: bool = actor
            .ask(CheckPermission {
                subject: "user:user-bob".to_string(),
                permission: "owner".to_string(),
                object: "waddle:test".to_string(),
            })
            .await
            .expect("check");
        assert!(!allowed);
    }

    #[tokio::test]
    async fn test_write_and_delete_tuple() {
        let actor = spawn_test_actor().await;

        // Write
        actor
            .ask(WriteTuple {
                tuple_str: "waddle:test#member@user:user-alice".to_string(),
            })
            .await
            .expect("write");

        // Verify it exists
        let allowed: bool = actor
            .ask(CheckPermission {
                subject: "user:user-alice".to_string(),
                permission: "member".to_string(),
                object: "waddle:test".to_string(),
            })
            .await
            .expect("check");
        assert!(allowed);

        // Delete
        actor
            .ask(DeleteTuple {
                tuple_str: "waddle:test#member@user:user-alice".to_string(),
            })
            .await
            .expect("delete");

        // Verify it no longer exists
        let allowed: bool = actor
            .ask(CheckPermission {
                subject: "user:user-alice".to_string(),
                permission: "member".to_string(),
                object: "waddle:test".to_string(),
            })
            .await
            .expect("check");
        assert!(!allowed);
    }

    #[tokio::test]
    async fn test_list_relations() {
        let actor = spawn_test_actor().await;

        // Write two relations for the same subject/object
        actor
            .ask(WriteTuple {
                tuple_str: "waddle:test#owner@user:user-alice".to_string(),
            })
            .await
            .expect("write owner");

        actor
            .ask(WriteTuple {
                tuple_str: "waddle:test#admin@user:user-alice".to_string(),
            })
            .await
            .expect("write admin");

        let relations: Vec<String> = actor
            .ask(ListRelations {
                subject: "user:user-alice".to_string(),
                object: "waddle:test".to_string(),
            })
            .await
            .expect("list_relations");

        assert_eq!(relations.len(), 2);
        assert!(relations.contains(&"owner".to_string()));
        assert!(relations.contains(&"admin".to_string()));
    }

    #[tokio::test]
    async fn test_list_subjects() {
        let actor = spawn_test_actor().await;

        // Write two members
        actor
            .ask(WriteTuple {
                tuple_str: "waddle:test#member@user:user-alice".to_string(),
            })
            .await
            .expect("write alice");

        actor
            .ask(WriteTuple {
                tuple_str: "waddle:test#member@user:user-bob".to_string(),
            })
            .await
            .expect("write bob");

        let subjects: Vec<String> = actor
            .ask(ListSubjects {
                object: "waddle:test".to_string(),
                relation: "member".to_string(),
            })
            .await
            .expect("list_subjects");

        assert_eq!(subjects.len(), 2);
        assert!(subjects.contains(&"user:user-alice".to_string()));
        assert!(subjects.contains(&"user:user-bob".to_string()));
    }

    #[tokio::test]
    async fn test_computed_permission_via_actor() {
        let actor = spawn_test_actor().await;

        // alice is owner of waddle:test
        actor
            .ask(WriteTuple {
                tuple_str: "waddle:test#owner@user:user-alice".to_string(),
            })
            .await
            .expect("write");

        // owner should have delete permission (computed via schema)
        let allowed: bool = actor
            .ask(CheckPermission {
                subject: "user:user-alice".to_string(),
                permission: "delete".to_string(),
                object: "waddle:test".to_string(),
            })
            .await
            .expect("check");
        assert!(allowed, "owner should have computed delete permission");
    }

    #[tokio::test]
    async fn test_invalid_tuple_format() {
        let actor = spawn_test_actor().await;

        let result = actor
            .ask(WriteTuple {
                tuple_str: "not-a-valid-tuple".to_string(),
            })
            .await;

        assert!(result.is_err(), "invalid tuple should return error");
    }

    #[tokio::test]
    async fn test_invalid_subject_in_check() {
        let actor = spawn_test_actor().await;

        let result = actor
            .ask(CheckPermission {
                subject: "bad-subject".to_string(),
                permission: "owner".to_string(),
                object: "waddle:test".to_string(),
            })
            .await;

        assert!(result.is_err(), "invalid subject should return error");
    }
}
