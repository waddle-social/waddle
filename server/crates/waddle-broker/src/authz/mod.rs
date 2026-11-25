//! Authorization backends (`PermissionBackend` implementations for OpenFGA and Local).

pub mod local;
pub mod openfga;
pub mod openfga_model;

use std::collections::HashSet;

use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, CheckDecision, CheckRequest, PermissionBackendError,
};

/// Workload caller identity determining which Modular Models an authorization
/// client may mutate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthzCallerRole {
    /// Cell workload: `writer` on `module:<store_id>|chat` and `reader` on `store:<store_id>`.
    Cell,
    /// Broker / authz authority: `writer` on both `chat` and `inference` modules.
    Broker,
}

/// Maps every `AuthzObjectType` to its owning OpenFGA Modular Model (`chat` vs `inference`).
pub const fn object_type_module(object_type: AuthzObjectType) -> AuthzModule {
    match object_type {
        AuthzObjectType::Server
        | AuthzObjectType::Space
        | AuthzObjectType::Channel
        | AuthzObjectType::Message
        | AuthzObjectType::DirectMessage
        | AuthzObjectType::Role => AuthzModule::Chat,
        AuthzObjectType::TenantCredential
        | AuthzObjectType::UserCredential
        | AuthzObjectType::InstalledExtension
        | AuthzObjectType::RoomInferencePolicy => AuthzModule::Inference,
    }
}

/// Enforces OpenFGA Modular Model write isolation before any RPC or local write:
/// 1. `AuthzCallerRole::Cell` can never write to `AuthzModule::Inference`.
/// 2. Every tuple's `object.object_type` MUST belong to `declared_module` so a
///    cell cannot smuggle an inference tuple under `AuthzModule::Chat`.
pub fn enforce_module_write(
    role: AuthzCallerRole,
    declared_module: AuthzModule,
    tuples: &[AuthzTuple],
) -> Result<(), PermissionBackendError> {
    if role == AuthzCallerRole::Cell && declared_module == AuthzModule::Inference {
        return Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference,
        ));
    }
    for tuple in tuples {
        let actual_module = object_type_module(tuple.object.object_type);
        if actual_module != declared_module {
            return Err(PermissionBackendError::ModuleWriteForbidden(actual_module));
        }
        if role == AuthzCallerRole::Cell && actual_module == AuthzModule::Inference {
            return Err(PermissionBackendError::ModuleWriteForbidden(
                AuthzModule::Inference,
            ));
        }
    }
    Ok(())
}

/// Enforces module write isolation for single-object exclusive relation mutations.
pub fn enforce_object_module_write(
    role: AuthzCallerRole,
    declared_module: AuthzModule,
    object: &AuthzObject,
) -> Result<(), PermissionBackendError> {
    let actual_module = object_type_module(object.object_type);
    if actual_module != declared_module {
        return Err(PermissionBackendError::ModuleWriteForbidden(actual_module));
    }
    if role == AuthzCallerRole::Cell && actual_module == AuthzModule::Inference {
        return Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference,
        ));
    }
    Ok(())
}

/// Shared Zanzibar / OpenFGA computed-permission evaluator used by
/// `LocalPermissionBackend`, the in-process OpenFGA HTTP engine, and the
/// in-memory SpiceDB transport so all three backends share exact schema v3 semantics.
pub fn evaluate_permission_in_memory(
    tuples: &HashSet<AuthzTuple>,
    req: &CheckRequest,
) -> CheckDecision {
    let mut visited = HashSet::new();
    if check_recursive(
        tuples,
        &req.object,
        req.permission.as_str(),
        &req.subject,
        &mut visited,
    ) {
        CheckDecision::Allowed
    } else {
        CheckDecision::Denied
    }
}

fn has_direct_tuple(
    tuples: &HashSet<AuthzTuple>,
    object: &AuthzObject,
    relation: &str,
    subject: &AuthzSubject,
) -> bool {
    tuples.contains(&AuthzTuple {
        object: object.clone(),
        relation: AuthzRelation::new(relation),
        subject: subject.clone(),
    })
}

fn related_objects(
    tuples: &HashSet<AuthzTuple>,
    object: &AuthzObject,
    relation: &str,
    expected_subject_type: AuthzSubjectType,
    target_object_type: AuthzObjectType,
) -> Vec<AuthzObject> {
    tuples
        .iter()
        .filter(|t| {
            t.object == *object
                && t.relation.as_str() == relation
                && t.subject.subject_type == expected_subject_type
        })
        .map(|t| AuthzObject {
            object_type: target_object_type,
            id: t.subject.id.clone(),
        })
        .collect()
}

fn check_recursive(
    tuples: &HashSet<AuthzTuple>,
    object: &AuthzObject,
    permission: &str,
    subject: &AuthzSubject,
    visited: &mut HashSet<(AuthzObjectType, String, String)>,
) -> bool {
    let key = (object.object_type, object.id.clone(), permission.to_owned());
    if !visited.insert(key) {
        return false;
    }

    if has_direct_tuple(tuples, object, permission, subject) {
        return true;
    }

    match (object.object_type, permission) {
        (AuthzObjectType::Space, "admin") => {
            check_recursive(tuples, object, "owner", subject, visited)
        }
        (AuthzObjectType::Space, "moderator") => {
            check_recursive(tuples, object, "admin", subject, visited)
        }
        (AuthzObjectType::Space, "member") => {
            check_recursive(tuples, object, "moderator", subject, visited)
        }
        (AuthzObjectType::Space, "read") => {
            check_recursive(tuples, object, "member", subject, visited)
        }
        (AuthzObjectType::Space, "manage") => {
            check_recursive(tuples, object, "admin", subject, visited)
        }

        (AuthzObjectType::Channel, "outcast") => {
            has_direct_tuple(tuples, object, "outcast", subject)
        }
        (AuthzObjectType::Channel, "owner") => {
            if check_recursive(tuples, object, "outcast", subject, visited) {
                return false;
            }
            related_objects(
                tuples,
                object,
                "space",
                AuthzSubjectType::Space,
                AuthzObjectType::Space,
            )
            .into_iter()
            .any(|space| check_recursive(tuples, &space, "owner", subject, visited))
        }
        (AuthzObjectType::Channel, "admin") => {
            if check_recursive(tuples, object, "outcast", subject, visited) {
                return false;
            }
            check_recursive(tuples, object, "owner", subject, visited)
                || related_objects(
                    tuples,
                    object,
                    "space",
                    AuthzSubjectType::Space,
                    AuthzObjectType::Space,
                )
                .into_iter()
                .any(|space| check_recursive(tuples, &space, "admin", subject, visited))
        }
        (AuthzObjectType::Channel, "member") => {
            if check_recursive(tuples, object, "outcast", subject, visited) {
                return false;
            }
            check_recursive(tuples, object, "admin", subject, visited)
                || related_objects(
                    tuples,
                    object,
                    "space",
                    AuthzSubjectType::Space,
                    AuthzObjectType::Space,
                )
                .into_iter()
                .any(|space| check_recursive(tuples, &space, "member", subject, visited))
        }
        (AuthzObjectType::Channel, "read") | (AuthzObjectType::Channel, "send_message") => {
            if check_recursive(tuples, object, "outcast", subject, visited) {
                return false;
            }
            check_recursive(tuples, object, "member", subject, visited)
        }

        (AuthzObjectType::Message, "read") => related_objects(
            tuples,
            object,
            "channel",
            AuthzSubjectType::Channel,
            AuthzObjectType::Channel,
        )
        .into_iter()
        .any(|ch| check_recursive(tuples, &ch, "read", subject, visited)),
        (AuthzObjectType::Message, "delete") => {
            has_direct_tuple(tuples, object, "author", subject)
                || related_objects(
                    tuples,
                    object,
                    "channel",
                    AuthzSubjectType::Channel,
                    AuthzObjectType::Channel,
                )
                .into_iter()
                .any(|ch| check_recursive(tuples, &ch, "admin", subject, visited))
        }

        (AuthzObjectType::DirectMessage, "read") => {
            has_direct_tuple(tuples, object, "participant", subject)
        }

        (AuthzObjectType::TenantCredential, "use") => {
            if has_direct_tuple(tuples, object, "revoked", subject) {
                return false;
            }
            has_direct_tuple(tuples, object, "allowed_extension", subject)
                || has_direct_tuple(tuples, object, "tenant_admin", subject)
        }
        (AuthzObjectType::TenantCredential, "manage") => {
            has_direct_tuple(tuples, object, "tenant_admin", subject)
        }

        (AuthzObjectType::UserCredential, "use") | (AuthzObjectType::UserCredential, "manage") => {
            has_direct_tuple(tuples, object, "owner", subject)
        }
        (AuthzObjectType::UserCredential, "invoke_with_consent") => {
            has_direct_tuple(tuples, object, "owner", subject)
                || has_direct_tuple(tuples, object, "consented_extension", subject)
        }

        (AuthzObjectType::InstalledExtension, "invoke") => {
            has_direct_tuple(tuples, object, "installer", subject)
                || related_objects(
                    tuples,
                    object,
                    "space",
                    AuthzSubjectType::Space,
                    AuthzObjectType::Space,
                )
                .into_iter()
                .any(|space| check_recursive(tuples, &space, "member", subject, visited))
        }

        (AuthzObjectType::RoomInferencePolicy, "allow_tenant_funded") => {
            if has_direct_tuple(tuples, object, "disabled", subject) {
                return false;
            }
            let channels = related_objects(
                tuples,
                object,
                "channel",
                AuthzSubjectType::Channel,
                AuthzObjectType::Channel,
            );
            if channels
                .iter()
                .any(|ch| check_recursive(tuples, ch, "outcast", subject, visited))
            {
                return false;
            }
            has_direct_tuple(tuples, object, "tenant_funded_allowed", subject)
                || channels
                    .iter()
                    .any(|ch| check_recursive(tuples, ch, "read", subject, visited))
        }
        (AuthzObjectType::RoomInferencePolicy, "allow_member_funded") => {
            if has_direct_tuple(tuples, object, "disabled", subject) {
                return false;
            }
            let channels = related_objects(
                tuples,
                object,
                "channel",
                AuthzSubjectType::Channel,
                AuthzObjectType::Channel,
            );
            if channels
                .iter()
                .any(|ch| check_recursive(tuples, ch, "outcast", subject, visited))
            {
                return false;
            }
            has_direct_tuple(tuples, object, "member_funded_allowed", subject)
                || channels
                    .iter()
                    .any(|ch| check_recursive(tuples, ch, "send_message", subject, visited))
        }

        _ => false,
    }
}
