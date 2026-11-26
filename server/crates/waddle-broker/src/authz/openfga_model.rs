//! OpenFGA Modular Models (`schema 1.2`) definitions and wire-format codecs.

use serde_json::json;
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzSubject, AuthzSubjectType,
    PermissionBackendError,
};

/// OpenFGA Modular Models require schema version `1.2`.
pub const OPENFGA_SCHEMA_VERSION: &str = "1.2";

/// OpenFGA `chat` module DSL (writable by cells on `module:<store_id>|chat`).
pub const OPENFGA_CHAT_MODULE_DSL: &str = r#"module chat
model
  schema 1.2

type user

type server
  relations
    define admin: [user]

type space
  relations
    define owner: [user]
    define admin: [user] or owner
    define moderator: [user] or admin
    define member: [user] or moderator
    define read: member
    define manage: admin

type channel
  relations
    define space: [space]
    define outcast: [user]
    define owner: [user] or owner from space but not outcast
    define admin: [user] or owner or admin from space but not outcast
    define member: [user] or admin or member from space but not outcast
    define read: member but not outcast
    define send_message: member but not outcast

type message
  relations
    define channel: [channel]
    define author: [user]
    define read: read from channel
    define delete: author or admin from channel

type direct_message
  relations
    define participant: [user]
    define read: participant

type role
  relations
    define space: [space]
    define assignee: [user]
"#;

/// OpenFGA `inference` module DSL (writable ONLY by broker/authz authority on
/// `module:<store_id>|inference`).
pub const OPENFGA_INFERENCE_MODULE_DSL: &str = r#"module inference
model
  schema 1.2

type extension

type tenant_credential
  relations
    define tenant_admin: [user]
    define allowed_extension: [extension]
    define revoked: [user]
    define use: (allowed_extension or tenant_admin) but not revoked
    define manage: tenant_admin

type user_credential
  relations
    define owner: [user]
    define consented_extension: [extension]
    define use: owner
    define invoke_with_consent: owner or consented_extension
    define manage: owner

type installed_extension
  relations
    define space: [space]
    define installer: [user]
    define invoke: installer or member from space

type room_inference_policy
  relations
    define channel: [channel]
    define tenant_funded_allowed: [user]
    define member_funded_allowed: [user]
    define disabled: [user]
    define allow_tenant_funded: (tenant_funded_allowed or read from channel) but not disabled but not outcast from channel
    define allow_member_funded: (member_funded_allowed or send_message from channel) but not disabled but not outcast from channel
"#;

/// OpenFGA Access Control object identifier for a module writer grant:
/// `module:<store_id>|chat` or `module:<store_id>|inference`.
pub fn openfga_module_grant_object(store_id: &str, module: AuthzModule) -> String {
    let suffix = match module {
        AuthzModule::Chat => "chat",
        AuthzModule::Inference => "inference",
    };
    format!("module:{store_id}|{suffix}")
}

/// OpenFGA Access Control object identifier for a store reader grant: `store:<store_id>`.
pub fn openfga_store_grant_object(store_id: &str) -> String {
    format!("store:{store_id}")
}

/// Canonical OpenFGA `schema 1.2` JSON authorization model submitted during
/// `POST /stores/{store_id}/authorization-models`.
pub fn openfga_authorization_model_json() -> serde_json::Value {
    json!({
        "schema_version": OPENFGA_SCHEMA_VERSION,
        "type_definitions": [
            { "type": "user", "metadata": { "module": "chat" } },
            { "type": "server", "metadata": { "module": "chat" }, "relations": { "admin": { "this": {} } } },
            { "type": "space", "metadata": { "module": "chat" }, "relations": { "owner": { "this": {} }, "admin": { "this": {} }, "moderator": { "this": {} }, "member": { "this": {} } } },
            { "type": "channel", "metadata": { "module": "chat" }, "relations": { "space": { "this": {} }, "outcast": { "this": {} }, "owner": { "this": {} }, "admin": { "this": {} }, "member": { "this": {} } } },
            { "type": "message", "metadata": { "module": "chat" }, "relations": { "channel": { "this": {} }, "author": { "this": {} } } },
            { "type": "direct_message", "metadata": { "module": "chat" }, "relations": { "participant": { "this": {} } } },
            { "type": "role", "metadata": { "module": "chat" }, "relations": { "space": { "this": {} }, "assignee": { "this": {} } } },
            { "type": "extension", "metadata": { "module": "inference" } },
            { "type": "tenant_credential", "metadata": { "module": "inference" }, "relations": { "tenant_admin": { "this": {} }, "allowed_extension": { "this": {} }, "revoked": { "this": {} } } },
            { "type": "user_credential", "metadata": { "module": "inference" }, "relations": { "owner": { "this": {} }, "consented_extension": { "this": {} } } },
            { "type": "installed_extension", "metadata": { "module": "inference" }, "relations": { "space": { "this": {} }, "installer": { "this": {} } } },
            { "type": "room_inference_policy", "metadata": { "module": "inference" }, "relations": { "channel": { "this": {} }, "tenant_funded_allowed": { "this": {} }, "member_funded_allowed": { "this": {} }, "disabled": { "this": {} } } }
        ]
    })
}

pub const fn object_type_wire_name(object_type: AuthzObjectType) -> &'static str {
    match object_type {
        AuthzObjectType::Server => "server",
        AuthzObjectType::Space => "space",
        AuthzObjectType::Channel => "channel",
        AuthzObjectType::Message => "message",
        AuthzObjectType::DirectMessage => "direct_message",
        AuthzObjectType::Role => "role",
        AuthzObjectType::TenantCredential => "tenant_credential",
        AuthzObjectType::UserCredential => "user_credential",
        AuthzObjectType::InstalledExtension => "installed_extension",
        AuthzObjectType::RoomInferencePolicy => "room_inference_policy",
    }
}

pub fn parse_object_type_wire_name(raw: &str) -> Result<AuthzObjectType, PermissionBackendError> {
    match raw {
        "server" => Ok(AuthzObjectType::Server),
        "space" => Ok(AuthzObjectType::Space),
        "channel" => Ok(AuthzObjectType::Channel),
        "message" => Ok(AuthzObjectType::Message),
        "direct_message" => Ok(AuthzObjectType::DirectMessage),
        "role" => Ok(AuthzObjectType::Role),
        "tenant_credential" => Ok(AuthzObjectType::TenantCredential),
        "user_credential" => Ok(AuthzObjectType::UserCredential),
        "installed_extension" => Ok(AuthzObjectType::InstalledExtension),
        "room_inference_policy" => Ok(AuthzObjectType::RoomInferencePolicy),
        other => Err(PermissionBackendError::Backend(format!(
            "unknown OpenFGA object type: {other}"
        ))),
    }
}

pub const fn subject_type_wire_name(subject_type: AuthzSubjectType) -> &'static str {
    match subject_type {
        AuthzSubjectType::Account => "user",
        AuthzSubjectType::Space => "space",
        AuthzSubjectType::Channel => "channel",
        AuthzSubjectType::Role => "role",
        AuthzSubjectType::Extension => "extension",
    }
}

pub fn parse_subject_type_wire_name(raw: &str) -> Result<AuthzSubjectType, PermissionBackendError> {
    match raw {
        "user" => Ok(AuthzSubjectType::Account),
        "space" => Ok(AuthzSubjectType::Space),
        "channel" => Ok(AuthzSubjectType::Channel),
        "role" => Ok(AuthzSubjectType::Role),
        "extension" => Ok(AuthzSubjectType::Extension),
        other => Err(PermissionBackendError::Backend(format!(
            "unknown OpenFGA subject type: {other}"
        ))),
    }
}

pub fn encode_openfga_object(object: &AuthzObject) -> String {
    format!(
        "{}:{}",
        object_type_wire_name(object.object_type),
        object.id
    )
}

pub fn decode_openfga_object(raw: &str) -> Result<AuthzObject, PermissionBackendError> {
    let (ty, id) = raw.split_once(':').ok_or_else(|| {
        PermissionBackendError::Backend(format!("invalid OpenFGA object identifier: {raw}"))
    })?;
    Ok(AuthzObject {
        object_type: parse_object_type_wire_name(ty)?,
        id: id.to_owned(),
    })
}

pub fn encode_openfga_subject(subject: &AuthzSubject) -> String {
    let ty = subject_type_wire_name(subject.subject_type);
    match &subject.relation {
        Some(rel) => format!("{ty}:{}#{rel}", subject.id),
        None => format!("{ty}:{}", subject.id),
    }
}

pub fn decode_openfga_subject(raw: &str) -> Result<AuthzSubject, PermissionBackendError> {
    let (ty, rest) = raw.split_once(':').ok_or_else(|| {
        PermissionBackendError::Backend(format!("invalid OpenFGA user identifier: {raw}"))
    })?;
    let (id, relation) = match rest.split_once('#') {
        Some((id, rel)) => (id.to_owned(), Some(rel.to_owned())),
        None => (rest.to_owned(), None),
    };
    Ok(AuthzSubject {
        subject_type: parse_subject_type_wire_name(ty)?,
        id,
        relation,
    })
}
