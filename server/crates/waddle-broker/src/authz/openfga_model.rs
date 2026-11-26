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

fn direct_type(ty: &str) -> serde_json::Value {
    json!({ "type": ty })
}

fn direct_meta(types: &[&str]) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = types.iter().map(|t| direct_type(t)).collect();
    json!({ "directly_related_user_types": entries })
}

fn computed(rel: &str) -> serde_json::Value {
    json!({ "computedUserset": { "relation": rel } })
}

fn ttu(tupleset: &str, computed_rel: &str) -> serde_json::Value {
    json!({
        "tupleToUserset": {
            "tupleset": { "relation": tupleset },
            "computedUserset": { "relation": computed_rel }
        }
    })
}

fn union(children: Vec<serde_json::Value>) -> serde_json::Value {
    json!({ "union": { "child": children } })
}

fn diff(base: serde_json::Value, subtract: serde_json::Value) -> serde_json::Value {
    json!({ "difference": { "base": base, "subtract": subtract } })
}

/// Canonical OpenFGA `schema 1.2` JSON authorization model submitted during
/// `POST /stores/{store_id}/authorization-models`.
pub fn openfga_authorization_model_json() -> serde_json::Value {
    let this = || json!({ "this": {} });
    json!({
        "schema_version": OPENFGA_SCHEMA_VERSION,
        "type_definitions": [
            { "type": "user", "metadata": { "module": "chat" } },
            {
                "type": "server",
                "relations": {
                    "owner": this(),
                    "admin": union(vec![this(), computed("owner")]),
                    "member": union(vec![this(), computed("admin")]),
                    "create_space": computed("owner"),
                    "create_muc": computed("owner")
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "owner": direct_meta(&["user"]),
                        "admin": direct_meta(&["user"]),
                        "member": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "space",
                "relations": {
                    "owner": this(),
                    "admin": union(vec![this(), computed("owner")]),
                    "moderator": union(vec![this(), computed("admin")]),
                    "member": union(vec![this(), computed("moderator")]),
                    "read": computed("member"),
                    "view": computed("member"),
                    "manage": computed("admin"),
                    "delete": computed("owner"),
                    "manage_settings": computed("admin"),
                    "manage_roles": computed("admin"),
                    "manage_members": computed("moderator"),
                    "create_channel": computed("owner"),
                    "update": computed("admin")
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "owner": direct_meta(&["user"]),
                        "admin": direct_meta(&["user"]),
                        "moderator": direct_meta(&["user"]),
                        "member": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "channel",
                "relations": {
                    "space": this(),
                    "parent": this(),
                    "outcast": this(),
                    "manager": this(),
                    "moderator": this(),
                    "writer": this(),
                    "viewer": this(),
                    "owner": diff(
                        union(vec![this(), ttu("space", "owner"), ttu("parent", "owner")]),
                        computed("outcast")
                    ),
                    "admin": diff(
                        union(vec![this(), computed("owner"), ttu("space", "admin"), ttu("parent", "admin")]),
                        computed("outcast")
                    ),
                    "member": diff(
                        union(vec![this(), computed("admin"), ttu("space", "member"), ttu("parent", "member")]),
                        computed("outcast")
                    ),
                    "delete": diff(
                        union(vec![ttu("space", "admin"), ttu("parent", "admin")]),
                        computed("outcast")
                    ),
                    "manage": diff(
                        union(vec![computed("owner"), computed("admin"), computed("manager"), ttu("space", "admin"), ttu("parent", "admin")]),
                        computed("outcast")
                    ),
                    "moderate": diff(
                        union(vec![computed("owner"), computed("admin"), computed("moderator"), computed("manager"), ttu("space", "moderator"), ttu("parent", "moderator")]),
                        computed("outcast")
                    ),
                    "send_message": diff(
                        union(vec![computed("member"), computed("writer"), computed("moderator"), computed("manager")]),
                        computed("outcast")
                    ),
                    "read": diff(
                        union(vec![computed("member"), computed("viewer"), computed("writer"), computed("moderator"), computed("manager")]),
                        computed("outcast")
                    ),
                    "view": computed("read")
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "space": direct_meta(&["space"]),
                        "parent": direct_meta(&["space"]),
                        "outcast": direct_meta(&["user"]),
                        "manager": direct_meta(&["user"]),
                        "moderator": direct_meta(&["user"]),
                        "writer": direct_meta(&["user"]),
                        "viewer": direct_meta(&["user"]),
                        "owner": direct_meta(&["user"]),
                        "admin": direct_meta(&["user"]),
                        "member": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "message",
                "relations": {
                    "channel": this(),
                    "author": this(),
                    "read": ttu("channel", "read"),
                    "react": ttu("channel", "read"),
                    "edit": diff(computed("author"), ttu("channel", "outcast")),
                    "delete": union(vec![
                        diff(computed("author"), ttu("channel", "outcast")),
                        ttu("channel", "moderate")
                    ])
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "channel": direct_meta(&["channel"]),
                        "author": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "direct_message",
                "relations": {
                    "participant": this(),
                    "owner": this(),
                    "read": computed("participant"),
                    "send": computed("participant"),
                    "leave": computed("participant"),
                    "manage": computed("owner"),
                    "add_participant": union(vec![computed("owner"), computed("participant")])
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "participant": direct_meta(&["user"]),
                        "owner": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "role",
                "relations": {
                    "space": this(),
                    "parent": this(),
                    "assignee": this(),
                    "member": this(),
                    "manager": this(),
                    "assign": union(vec![computed("manager"), ttu("space", "admin"), ttu("parent", "admin")]),
                    "manage": union(vec![computed("manager"), ttu("space", "admin"), ttu("parent", "admin")]),
                    "delete": union(vec![computed("manager"), ttu("space", "admin"), ttu("parent", "admin")]),
                    "edit": union(vec![computed("manager"), ttu("space", "admin"), ttu("parent", "admin")])
                },
                "metadata": {
                    "module": "chat",
                    "relations": {
                        "space": direct_meta(&["space"]),
                        "parent": direct_meta(&["space"]),
                        "assignee": direct_meta(&["user"]),
                        "member": direct_meta(&["user"]),
                        "manager": direct_meta(&["user"])
                    }
                }
            },
            { "type": "extension", "metadata": { "module": "inference" } },
            {
                "type": "tenant_credential",
                "relations": {
                    "tenant_admin": this(),
                    "allowed_extension": this(),
                    "revoked": this(),
                    "use": diff(
                        union(vec![computed("allowed_extension"), computed("tenant_admin")]),
                        computed("revoked")
                    ),
                    "manage": computed("tenant_admin")
                },
                "metadata": {
                    "module": "inference",
                    "relations": {
                        "tenant_admin": direct_meta(&["user"]),
                        "allowed_extension": direct_meta(&["extension"]),
                        "revoked": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "user_credential",
                "relations": {
                    "owner": this(),
                    "consented_extension": this(),
                    "use": computed("owner"),
                    "invoke_with_consent": union(vec![computed("owner"), computed("consented_extension")]),
                    "manage": computed("owner")
                },
                "metadata": {
                    "module": "inference",
                    "relations": {
                        "owner": direct_meta(&["user"]),
                        "consented_extension": direct_meta(&["extension"])
                    }
                }
            },
            {
                "type": "installed_extension",
                "relations": {
                    "space": this(),
                    "installer": this(),
                    "invoke": union(vec![computed("installer"), ttu("space", "member")])
                },
                "metadata": {
                    "module": "inference",
                    "relations": {
                        "space": direct_meta(&["space"]),
                        "installer": direct_meta(&["user"])
                    }
                }
            },
            {
                "type": "room_inference_policy",
                "relations": {
                    "channel": this(),
                    "tenant_funded_allowed": this(),
                    "member_funded_allowed": this(),
                    "disabled": this(),
                    "allow_tenant_funded": diff(
                        union(vec![computed("tenant_funded_allowed"), ttu("channel", "read")]),
                        union(vec![computed("disabled"), ttu("channel", "outcast")])
                    ),
                    "allow_member_funded": diff(
                        union(vec![computed("member_funded_allowed"), ttu("channel", "send_message")]),
                        union(vec![computed("disabled"), ttu("channel", "outcast")])
                    )
                },
                "metadata": {
                    "module": "inference",
                    "relations": {
                        "channel": direct_meta(&["channel"]),
                        "tenant_funded_allowed": direct_meta(&["user"]),
                        "member_funded_allowed": direct_meta(&["user"]),
                        "disabled": direct_meta(&["user"])
                    }
                }
            }
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
