//! Permission check algorithm
//!
//! Implements the Zanzibar-style permission check algorithm with:
//! - Direct tuple lookups
//! - Computed permissions (union, intersection, arrow)
//! - Userset expansion
//! - LRU caching for performance

use std::collections::HashSet;
use std::sync::Arc;

use lru::LruCache;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, instrument};

use super::schema::{ComputedPermission, PermissionSchema};
use super::tuple::{Object, ObjectType, Subject, SubjectType, TupleStore};
use super::PermissionError;
use crate::db::Database;

/// Maximum depth for permission check traversal (prevents infinite loops)
const MAX_CHECK_DEPTH: usize = 10;

/// Default cache size
const DEFAULT_CACHE_SIZE: usize = 1000;

/// Request to check a permission
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckRequest {
    pub subject: Subject,
    pub permission: String,
    pub object: Object,
}

impl CheckRequest {
    /// Create a new check request
    #[allow(dead_code)]
    pub fn new(subject: Subject, permission: impl Into<String>, object: Object) -> Self {
        Self {
            subject,
            permission: permission.into(),
            object,
        }
    }
}

/// Response from a permission check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResponse {
    pub allowed: bool,
    pub reason: Option<String>,
}

impl CheckResponse {
    /// Create an allowed response
    pub fn allowed(reason: impl Into<String>) -> Self {
        Self {
            allowed: true,
            reason: Some(reason.into()),
        }
    }

    /// Create a denied response
    pub fn denied() -> Self {
        Self {
            allowed: false,
            reason: None,
        }
    }
}

/// Permission checker with schema and optional caching
pub struct PermissionChecker {
    tuple_store: TupleStore,
    schema: PermissionSchema,
    cache: Arc<Mutex<LruCache<CheckRequest, bool>>>,
}

impl PermissionChecker {
    /// Create a new permission checker
    pub fn new(db: Arc<Database>, schema: PermissionSchema) -> Self {
        Self {
            tuple_store: TupleStore::new(db),
            schema,
            cache: Arc::new(Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(DEFAULT_CACHE_SIZE).unwrap(),
            ))),
        }
    }

    /// Create a new permission checker with custom cache size
    #[allow(dead_code)]
    pub fn with_cache_size(db: Arc<Database>, schema: PermissionSchema, cache_size: usize) -> Self {
        Self {
            tuple_store: TupleStore::new(db),
            schema,
            cache: Arc::new(Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(cache_size.max(1)).unwrap(),
            ))),
        }
    }

    /// Check a permission
    #[instrument(skip(self), fields(subject = %request.subject, permission = %request.permission, object = %request.object))]
    pub async fn check(&self, request: CheckRequest) -> Result<CheckResponse, PermissionError> {
        // Check cache first
        {
            let mut cache = self.cache.lock().await;
            if let Some(&cached) = cache.get(&request) {
                debug!("Cache hit for permission check");
                return Ok(if cached {
                    CheckResponse::allowed("cached")
                } else {
                    CheckResponse::denied()
                });
            }
        }

        // Perform the check
        let mut visited = HashSet::new();
        let result = self
            .check_internal(&request.subject, &request.permission, &request.object, 0, &mut visited)
            .await?;

        // Update cache
        {
            let mut cache = self.cache.lock().await;
            cache.put(request, result.allowed);
        }

        Ok(result)
    }

    /// Internal recursive check implementation
    async fn check_internal(
        &self,
        subject: &Subject,
        permission: &str,
        object: &Object,
        depth: usize,
        visited: &mut HashSet<String>,
    ) -> Result<CheckResponse, PermissionError> {
        // Prevent infinite loops
        if depth > MAX_CHECK_DEPTH {
            return Err(PermissionError::MaxDepthExceeded(MAX_CHECK_DEPTH));
        }

        // Create a visit key to prevent cycles
        let visit_key = format!("{}#{}@{}", object, permission, subject);
        if visited.contains(&visit_key) {
            debug!("Cycle detected, returning denied");
            return Ok(CheckResponse::denied());
        }
        visited.insert(visit_key);

        debug!(
            "Checking permission: {} has {} on {} (depth: {})",
            subject, permission, object, depth
        );

        // 1. Check for direct relation (permission name equals relation name)
        if self
            .tuple_store
            .exists(object, permission, subject)
            .await?
        {
            debug!("Direct relation found: {} has {} on {}", subject, permission, object);
            return Ok(CheckResponse::allowed(format!("direct:{}", permission)));
        }

        // 2. Check for userset expansion
        // If the subject is a user, check if they're part of any userset that has the permission
        if subject.subject_type == SubjectType::User {
            let subjects = self.tuple_store.list_subjects(object, permission).await?;
            for tuple_subject in subjects {
                if tuple_subject.is_userset() {
                    // Check if the user is a member of this userset
                    let userset_object = Object::new(
                        match tuple_subject.subject_type {
                            SubjectType::User => continue, // Users can't be usersets
                            SubjectType::Waddle => ObjectType::Waddle,
                            SubjectType::Role => ObjectType::Role,
                        },
                        &tuple_subject.id,
                    );
                    let userset_relation = tuple_subject.relation.as_ref().unwrap();

                    if self
                        .tuple_store
                        .exists(&userset_object, userset_relation, subject)
                        .await?
                    {
                        debug!(
                            "Userset match: {} is {} of {}, which has {} on {}",
                            subject, userset_relation, userset_object, permission, object
                        );
                        return Ok(CheckResponse::allowed(format!(
                            "userset:{}#{}",
                            userset_object, userset_relation
                        )));
                    }
                }
            }
        }

        // 3. Check computed permissions from schema
        if let Some(computation) = self.schema.get_permission(object.object_type, permission) {
            let result = self
                .check_computed(subject, computation, object, depth, visited)
                .await?;
            if result.allowed {
                return Ok(result);
            }
        }

        Ok(CheckResponse::denied())
    }

    /// Check a computed permission
    fn check_computed<'a>(
        &'a self,
        subject: &'a Subject,
        computation: &'a ComputedPermission,
        object: &'a Object,
        depth: usize,
        visited: &'a mut HashSet<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CheckResponse, PermissionError>> + Send + 'a>> {
        Box::pin(async move {
            match computation {
                ComputedPermission::DirectRelation(relation) => {
                    // Check for direct tuple with this relation
                    if self.tuple_store.exists(object, relation, subject).await? {
                        return Ok(CheckResponse::allowed(format!("relation:{}", relation)));
                    }

                    // Also check userset expansion for this relation
                    if subject.subject_type == SubjectType::User {
                        let subjects = self.tuple_store.list_subjects(object, relation).await?;
                        for tuple_subject in subjects {
                            if tuple_subject.is_userset() {
                                let userset_object = Object::new(
                                    match tuple_subject.subject_type {
                                        SubjectType::User => continue,
                                        SubjectType::Waddle => ObjectType::Waddle,
                                        SubjectType::Role => ObjectType::Role,
                                    },
                                    &tuple_subject.id,
                                );
                                let userset_relation = tuple_subject.relation.as_ref().unwrap();

                                if self
                                    .tuple_store
                                    .exists(&userset_object, userset_relation, subject)
                                    .await?
                                {
                                    return Ok(CheckResponse::allowed(format!(
                                        "userset:{}#{}",
                                        userset_object, userset_relation
                                    )));
                                }
                            }
                        }
                    }

                    Ok(CheckResponse::denied())
                }

                ComputedPermission::Union(permissions) => {
                    // Any permission in the union grants access
                    for perm in permissions {
                        let result = self
                            .check_computed(subject, perm, object, depth + 1, visited)
                            .await?;
                        if result.allowed {
                            return Ok(result);
                        }
                    }
                    Ok(CheckResponse::denied())
                }

                ComputedPermission::Intersection(permissions) => {
                    // All permissions must be satisfied
                    for perm in permissions {
                        let result = self
                            .check_computed(subject, perm, object, depth + 1, visited)
                            .await?;
                        if !result.allowed {
                            return Ok(CheckResponse::denied());
                        }
                    }
                    Ok(CheckResponse::allowed("intersection"))
                }

                ComputedPermission::Arrow(relation, target_permission) => {
                    // Follow the relation to find parent objects and check permission there
                    let parent_tuples = self
                        .tuple_store
                        .get_tuples_for_object(object, Some(relation))
                        .await?;

                    for tuple in parent_tuples {
                        // The subject of the tuple is the parent object
                        let parent_object = Object::new(
                            match tuple.subject.subject_type {
                                SubjectType::User => continue, // Users can't be parent objects
                                SubjectType::Waddle => ObjectType::Waddle,
                                SubjectType::Role => ObjectType::Role,
                            },
                            &tuple.subject.id,
                        );

                        debug!(
                            "Following arrow: {} -> {} on {}",
                            relation, target_permission, parent_object
                        );

                        let result = self
                            .check_internal(subject, target_permission, &parent_object, depth + 1, visited)
                            .await?;

                        if result.allowed {
                            return Ok(CheckResponse::allowed(format!(
                                "arrow:{}->{}",
                                relation, target_permission
                            )));
                        }
                    }

                    Ok(CheckResponse::denied())
                }
            }
        })
    }

    /// Invalidate cache entries for a specific object
    #[allow(dead_code)]
    pub async fn invalidate_object(&self, object: &Object) {
        let mut cache = self.cache.lock().await;
        // Note: LruCache doesn't support partial key matching, so we clear all
        // In a production system, you'd want a more sophisticated cache
        cache.clear();
        debug!("Invalidated cache for object: {}", object);
    }

    /// Clear the entire cache
    #[allow(dead_code)]
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.lock().await;
        cache.clear();
        debug!("Cleared permission cache");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MigrationRunner;
    use crate::permissions::tuple::{Relation, Tuple};

    async fn setup_test_db() -> (Arc<Database>, TupleStore) {
        let db = Database::in_memory("test-check").await.unwrap();
        let db = Arc::new(db);

        let runner = MigrationRunner::global();
        runner.run(&db).await.unwrap();

        let store = TupleStore::new(Arc::clone(&db));
        (db, store)
    }

    #[tokio::test]
    async fn test_direct_permission_check() {
        let (db, store) = setup_test_db().await;
        let checker = PermissionChecker::new(Arc::clone(&db), PermissionSchema::default());

        // Create tuple: alice is owner of waddle:test
        let tuple = Tuple::new(
            Object::new(ObjectType::Waddle, "test"),
            Relation::new("owner"),
            Subject::user("did:plc:alice"),
        );
        store.write(tuple).await.unwrap();

        // Check: alice has owner permission on waddle:test
        let request = CheckRequest::new(
            Subject::user("did:plc:alice"),
            "owner",
            Object::new(ObjectType::Waddle, "test"),
        );
        let response = checker.check(request).await.unwrap();
        assert!(response.allowed);

        // Check: bob does NOT have owner permission
        let request = CheckRequest::new(
            Subject::user("did:plc:bob"),
            "owner",
            Object::new(ObjectType::Waddle, "test"),
        );
        let response = checker.check(request).await.unwrap();
        assert!(!response.allowed);
    }

    #[tokio::test]
    async fn test_computed_permission_union() {
        let (db, store) = setup_test_db().await;
        let checker = PermissionChecker::new(Arc::clone(&db), PermissionSchema::default());

        // Create tuple: alice is admin of waddle:test (not owner)
        let tuple = Tuple::new(
            Object::new(ObjectType::Waddle, "test"),
            Relation::new("admin"),
            Subject::user("did:plc:alice"),
        );
        store.write(tuple).await.unwrap();

        // Check: alice has manage_settings permission (granted to owner OR admin)
        let request = CheckRequest::new(
            Subject::user("did:plc:alice"),
            "manage_settings",
            Object::new(ObjectType::Waddle, "test"),
        );
        let response = checker.check(request).await.unwrap();
        assert!(response.allowed);
    }

    #[tokio::test]
    async fn test_arrow_permission() {
        let (db, store) = setup_test_db().await;
        let checker = PermissionChecker::new(Arc::clone(&db), PermissionSchema::default());

        // Setup:
        // 1. alice is admin of waddle:test
        // 2. channel:general has parent waddle:test
        store
            .write(Tuple::new(
                Object::new(ObjectType::Waddle, "test"),
                Relation::new("admin"),
                Subject::user("did:plc:alice"),
            ))
            .await
            .unwrap();

        store
            .write(Tuple::new(
                Object::new(ObjectType::Channel, "general"),
                Relation::new("parent"),
                Subject::userset(SubjectType::Waddle, "test", ""),
            ))
            .await
            .unwrap();

        // Check: alice can delete channel:general (requires parent->admin)
        let request = CheckRequest::new(
            Subject::user("did:plc:alice"),
            "delete",
            Object::new(ObjectType::Channel, "general"),
        );
        let response = checker.check(request).await.unwrap();
        assert!(response.allowed, "Alice should be able to delete channel via arrow permission");
    }

    #[tokio::test]
    async fn test_inherited_permission_via_membership() {
        let (db, store) = setup_test_db().await;
        let checker = PermissionChecker::new(Arc::clone(&db), PermissionSchema::default());

        // Setup:
        // 1. alice is a member of waddle:test
        // 2. channel:general has parent waddle:test
        store
            .write(Tuple::new(
                Object::new(ObjectType::Waddle, "test"),
                Relation::new("member"),
                Subject::user("did:plc:alice"),
            ))
            .await
            .unwrap();

        store
            .write(Tuple::new(
                Object::new(ObjectType::Channel, "general"),
                Relation::new("parent"),
                Subject::userset(SubjectType::Waddle, "test", ""),
            ))
            .await
            .unwrap();

        // Check: alice can view channel:general (via parent->member)
        let request = CheckRequest::new(
            Subject::user("did:plc:alice"),
            "view",
            Object::new(ObjectType::Channel, "general"),
        );
        let response = checker.check(request).await.unwrap();
        assert!(
            response.allowed,
            "Waddle member should be able to view channel through inheritance"
        );
    }

    #[tokio::test]
    async fn test_userset_permission() {
        let (db, store) = setup_test_db().await;
        let checker = PermissionChecker::new(Arc::clone(&db), PermissionSchema::default());

        // Setup:
        // 1. alice is a member of waddle:test
        // 2. channel:general grants viewer to waddle:test#member (all members)
        store
            .write(Tuple::new(
                Object::new(ObjectType::Waddle, "test"),
                Relation::new("member"),
                Subject::user("did:plc:alice"),
            ))
            .await
            .unwrap();

        store
            .write(Tuple::new(
                Object::new(ObjectType::Channel, "general"),
                Relation::new("viewer"),
                Subject::userset(SubjectType::Waddle, "test", "member"),
            ))
            .await
            .unwrap();

        // Check: alice has viewer permission via userset
        let request = CheckRequest::new(
            Subject::user("did:plc:alice"),
            "viewer",
            Object::new(ObjectType::Channel, "general"),
        );
        let response = checker.check(request).await.unwrap();
        assert!(response.allowed, "Alice should have viewer via userset membership");
    }

    #[tokio::test]
    async fn test_cache() {
        let (db, store) = setup_test_db().await;
        let checker = PermissionChecker::new(Arc::clone(&db), PermissionSchema::default());

        // Create tuple
        store
            .write(Tuple::new(
                Object::new(ObjectType::Waddle, "test"),
                Relation::new("owner"),
                Subject::user("did:plc:alice"),
            ))
            .await
            .unwrap();

        // First check
        let request = CheckRequest::new(
            Subject::user("did:plc:alice"),
            "owner",
            Object::new(ObjectType::Waddle, "test"),
        );
        let response1 = checker.check(request.clone()).await.unwrap();
        assert!(response1.allowed);

        // Second check should be cached
        let response2 = checker.check(request).await.unwrap();
        assert!(response2.allowed);
        assert_eq!(response2.reason, Some("cached".to_string()));
    }

    #[tokio::test]
    async fn test_owner_has_delete() {
        let (db, store) = setup_test_db().await;
        let checker = PermissionChecker::new(Arc::clone(&db), PermissionSchema::default());

        // Create owner tuple
        store
            .write(Tuple::new(
                Object::new(ObjectType::Waddle, "test"),
                Relation::new("owner"),
                Subject::user("did:plc:alice"),
            ))
            .await
            .unwrap();

        // Check delete permission (computed from owner relation)
        let request = CheckRequest::new(
            Subject::user("did:plc:alice"),
            "delete",
            Object::new(ObjectType::Waddle, "test"),
        );
        let response = checker.check(request).await.unwrap();
        assert!(response.allowed, "Owner should have delete permission");
    }
}
