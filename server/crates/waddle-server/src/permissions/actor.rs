use kameo::message::Context;
use kameo::Actor;

use crate::config::ServerConfig;
use crate::db::actor::DbActor;
use crate::db::Database;
use std::sync::Arc;

use super::spicedb::SpiceDbPermissionBackend;
use super::{CheckRequest, PermissionChecker, PermissionSchema, TupleStore};
use super::{
    CheckResponse, Object, ObjectType, PermissionError, Relation, Subject, SubjectType, Tuple,
};

enum PermissionActorBackend {
    SpiceDb(Box<SpiceDbPermissionBackend>),
    Local {
        tuple_store: TupleStore,
        checker: PermissionChecker,
    },
}

/// Typed permission names used by runtime actor messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub enum Permission {
    Owner,
    Admin,
    Moderator,
    Member,
    Delete,
    ManageSettings,
    ManageRoles,
    ManageMembers,
    CreateChannel,
    Update,
    View,
    Manage,
    Moderate,
    SendMessage,
    Read,
    Edit,
    React,
    Send,
    AddParticipant,
    Leave,
    Assign,
    Custom(String),
}

impl Permission {
    pub fn as_str(&self) -> &str {
        match self {
            Permission::Owner => "owner",
            Permission::Admin => "admin",
            Permission::Moderator => "moderator",
            Permission::Member => "member",
            Permission::Delete => "delete",
            Permission::ManageSettings => "manage_settings",
            Permission::ManageRoles => "manage_roles",
            Permission::ManageMembers => "manage_members",
            Permission::CreateChannel => "create_channel",
            Permission::Update => "update",
            Permission::View => "view",
            Permission::Manage => "manage",
            Permission::Moderate => "moderate",
            Permission::SendMessage => "send_message",
            Permission::Read => "read",
            Permission::Edit => "edit",
            Permission::React => "react",
            Permission::Send => "send",
            Permission::AddParticipant => "add_participant",
            Permission::Leave => "leave",
            Permission::Assign => "assign",
            Permission::Custom(value) => value.as_str(),
        }
    }
}

/// Actor that owns the permission backend directly and handles operations via
/// typed message passing.
#[derive(Actor)]
pub struct PermissionActor {
    backend: PermissionActorBackend,
}

impl PermissionActor {
    pub fn new(backend: SpiceDbPermissionBackend) -> Self {
        Self {
            backend: PermissionActorBackend::SpiceDb(Box::new(backend)),
        }
    }

    pub async fn from_server_config(config: &ServerConfig) -> Result<Self, PermissionError> {
        let spicedb = config
            .spicedb
            .as_ref()
            .ok_or(PermissionError::SpiceDbConfigMissing)?;
        let backend = SpiceDbPermissionBackend::connect(spicedb).await?;
        Ok(Self::new(backend))
    }

    pub fn new_for_tests(db: Arc<Database>) -> Self {
        let actor = kameo::spawn(DbActor::new((*db).clone()));
        let tuple_store = TupleStore::new(actor.clone());
        let schema = PermissionSchema::default();
        let checker = PermissionChecker::new(actor, schema);

        Self {
            backend: PermissionActorBackend::Local {
                tuple_store,
                checker,
            },
        }
    }

    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            PermissionActorBackend::SpiceDb(_) => "spicedb",
            PermissionActorBackend::Local { .. } => "local",
        }
    }

    async fn check(
        &self,
        subject: &Subject,
        permission: &str,
        object: &Object,
    ) -> Result<CheckResponse, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend.check(subject, permission, object).await
            }
            PermissionActorBackend::Local { checker, .. } => {
                checker
                    .check(CheckRequest {
                        subject: subject.clone(),
                        permission: permission.to_string(),
                        object: object.clone(),
                    })
                    .await
            }
        }
    }

    async fn write_tuple(&self, tuple: Tuple) -> Result<(), PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => backend.write_tuple(tuple).await,
            PermissionActorBackend::Local { tuple_store, .. } => tuple_store.write(tuple).await,
        }
    }

    async fn delete_tuple(&self, tuple: &Tuple) -> Result<(), PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => backend.delete_tuple(tuple).await,
            PermissionActorBackend::Local { tuple_store, .. } => tuple_store.delete(tuple).await,
        }
    }

    async fn list_relations(
        &self,
        subject: &Subject,
        object: &Object,
    ) -> Result<Vec<String>, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend.list_relations(subject, object).await
            }
            PermissionActorBackend::Local { tuple_store, .. } => {
                tuple_store.list_relations(subject, object).await
            }
        }
    }

    async fn list_subjects(
        &self,
        object: &Object,
        relation: &str,
    ) -> Result<Vec<Subject>, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend.list_subjects(object, relation).await
            }
            PermissionActorBackend::Local { tuple_store, .. } => {
                tuple_store.list_subjects(object, relation).await
            }
        }
    }

    async fn lookup_resources(
        &self,
        subject: &Subject,
        permission: &str,
        object_type: ObjectType,
    ) -> Result<Vec<Object>, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend
                    .lookup_resources(subject, permission, object_type)
                    .await
            }
            PermissionActorBackend::Local { .. } => {
                Err(PermissionError::UnsupportedOperation("local"))
            }
        }
    }

    async fn lookup_subjects(
        &self,
        object: &Object,
        permission: &str,
        subject_type: SubjectType,
    ) -> Result<Vec<Subject>, PermissionError> {
        match &self.backend {
            PermissionActorBackend::SpiceDb(backend) => {
                backend
                    .lookup_subjects(object, permission, subject_type)
                    .await
            }
            PermissionActorBackend::Local { .. } => {
                Err(PermissionError::UnsupportedOperation("local"))
            }
        }
    }

    async fn clear_cache(&self) {
        if let PermissionActorBackend::Local { checker, .. } = &self.backend {
            checker.clear_cache().await;
        }
    }
}

/// Check whether a subject has a permission on an object.
pub struct CheckPermission {
    pub subject: Subject,
    pub permission: Permission,
    pub object: Object,
}

impl kameo::message::Message<CheckPermission> for PermissionActor {
    type Reply = Result<CheckResponse, PermissionError>;

    async fn handle(
        &mut self,
        msg: CheckPermission,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.check(&msg.subject, msg.permission.as_str(), &msg.object)
            .await
    }
}

/// Write a permission tuple.
pub struct WriteTuple {
    pub tuple: Tuple,
}

impl kameo::message::Message<WriteTuple> for PermissionActor {
    type Reply = Result<(), PermissionError>;

    async fn handle(
        &mut self,
        msg: WriteTuple,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.write_tuple(msg.tuple).await?;
        self.clear_cache().await;
        Ok(())
    }
}

/// Delete a permission tuple.
pub struct DeleteTuple {
    pub tuple: Tuple,
}

impl kameo::message::Message<DeleteTuple> for PermissionActor {
    type Reply = Result<(), PermissionError>;

    async fn handle(
        &mut self,
        msg: DeleteTuple,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.delete_tuple(&msg.tuple).await?;
        self.clear_cache().await;
        Ok(())
    }
}

/// List direct relations a subject has on an object.
pub struct ListRelations {
    pub subject: Subject,
    pub object: Object,
}

impl kameo::message::Message<ListRelations> for PermissionActor {
    type Reply = Result<Vec<Relation>, PermissionError>;

    async fn handle(
        &mut self,
        msg: ListRelations,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let relations = self.list_relations(&msg.subject, &msg.object).await?;
        Ok(relations.into_iter().map(Relation::new).collect())
    }
}

/// List subjects that have a relation on an object.
pub struct ListSubjects {
    pub object: Object,
    pub relation: Relation,
}

impl kameo::message::Message<ListSubjects> for PermissionActor {
    type Reply = Result<Vec<Subject>, PermissionError>;

    async fn handle(
        &mut self,
        msg: ListSubjects,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.list_subjects(&msg.object, &msg.relation.name).await
    }
}

/// Lookup resources a subject may access for a permission.
pub struct LookupResources {
    pub subject: Subject,
    pub permission: Permission,
    pub object_type: ObjectType,
}

impl kameo::message::Message<LookupResources> for PermissionActor {
    type Reply = Result<Vec<Object>, PermissionError>;

    async fn handle(
        &mut self,
        msg: LookupResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.lookup_resources(&msg.subject, msg.permission.as_str(), msg.object_type)
            .await
    }
}

/// Lookup subjects that may access an object for a permission.
pub struct LookupSubjects {
    pub object: Object,
    pub permission: Permission,
    pub subject_type: SubjectType,
}

impl kameo::message::Message<LookupSubjects> for PermissionActor {
    type Reply = Result<Vec<Subject>, PermissionError>;

    async fn handle(
        &mut self,
        msg: LookupSubjects,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.lookup_subjects(&msg.object, msg.permission.as_str(), msg.subject_type)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kameo::actor::ActorRef;

    use super::*;
    use crate::config::ServerConfig;
    use crate::db::{Database, MigrationRunner};

    async fn spawn_test_actor() -> ActorRef<PermissionActor> {
        let db = Database::in_memory("test-permission-actor")
            .await
            .expect("db");
        let db = Arc::new(db);

        let runner = MigrationRunner::global();
        runner.run(&db).await.expect("migrations");

        kameo::spawn(PermissionActor::new_for_tests(db))
    }

    #[tokio::test]
    async fn write_and_check_permission_with_typed_payloads() {
        let actor = spawn_test_actor().await;
        let tuple = Tuple::new(
            Object::new(ObjectType::Space, "test-space"),
            Relation::new("owner"),
            Subject::user("user-alice"),
        );

        actor
            .ask(WriteTuple { tuple })
            .await
            .expect("write should succeed");

        let response = actor
            .ask(CheckPermission {
                subject: Subject::user("user-alice"),
                permission: Permission::Delete,
                object: Object::new(ObjectType::Space, "test-space"),
            })
            .await
            .expect("check should succeed");

        assert!(response.allowed);
    }

    #[tokio::test]
    async fn write_and_delete_tuple_with_typed_payloads() {
        let actor = spawn_test_actor().await;
        let tuple = Tuple::new(
            Object::new(ObjectType::Space, "test-space"),
            Relation::new("member"),
            Subject::user("user-alice"),
        );

        actor
            .ask(WriteTuple {
                tuple: tuple.clone(),
            })
            .await
            .expect("write should succeed");

        actor
            .ask(DeleteTuple {
                tuple: tuple.clone(),
            })
            .await
            .expect("delete should succeed");

        let response = actor
            .ask(CheckPermission {
                subject: Subject::user("user-alice"),
                permission: Permission::Member,
                object: Object::new(ObjectType::Space, "test-space"),
            })
            .await
            .expect("check should succeed");

        assert!(!response.allowed);
    }

    #[tokio::test]
    async fn list_relations_returns_typed_relations() {
        let actor = spawn_test_actor().await;

        actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Space, "test-space"),
                    Relation::new("owner"),
                    Subject::user("user-alice"),
                ),
            })
            .await
            .expect("write owner");

        actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Space, "test-space"),
                    Relation::new("admin"),
                    Subject::user("user-alice"),
                ),
            })
            .await
            .expect("write admin");

        let relations = actor
            .ask(ListRelations {
                subject: Subject::user("user-alice"),
                object: Object::new(ObjectType::Space, "test-space"),
            })
            .await
            .expect("list relations should succeed");

        assert_eq!(relations.len(), 2);
        assert!(relations.contains(&Relation::new("owner")));
        assert!(relations.contains(&Relation::new("admin")));
    }

    #[tokio::test]
    async fn list_subjects_returns_typed_subjects() {
        let actor = spawn_test_actor().await;

        actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Space, "test-space"),
                    Relation::new("member"),
                    Subject::user("user-alice"),
                ),
            })
            .await
            .expect("write alice");

        actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Space, "test-space"),
                    Relation::new("member"),
                    Subject::user("user-bob"),
                ),
            })
            .await
            .expect("write bob");

        let subjects = actor
            .ask(ListSubjects {
                object: Object::new(ObjectType::Space, "test-space"),
                relation: Relation::new("member"),
            })
            .await
            .expect("list subjects should succeed");

        assert_eq!(subjects.len(), 2);
        assert!(subjects.contains(&Subject::user("user-alice")));
        assert!(subjects.contains(&Subject::user("user-bob")));
    }

    #[tokio::test]
    async fn duplicate_tuple_returns_typed_error() {
        let actor = spawn_test_actor().await;
        let tuple = Tuple::new(
            Object::new(ObjectType::Space, "test-space"),
            Relation::new("owner"),
            Subject::user("user-alice"),
        );

        actor
            .ask(WriteTuple {
                tuple: tuple.clone(),
            })
            .await
            .expect("first write should succeed");

        let result = actor.ask(WriteTuple { tuple }).await;
        assert!(result.is_err(), "duplicate tuple should return an error");
    }

    #[tokio::test]
    async fn from_server_config_requires_spicedb_config() {
        let config = ServerConfig::default();
        let result = PermissionActor::from_server_config(&config).await;

        assert!(matches!(result, Err(PermissionError::SpiceDbConfigMissing)));
    }
}
