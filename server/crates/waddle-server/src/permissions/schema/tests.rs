use super::*;

#[test]
fn test_space_schema() {
    let schema = PermissionSchema::default();
    let space_schema = schema.get_schema(ObjectType::Space).unwrap();

    // Check relations
    assert!(space_schema.is_valid_relation("owner"));
    assert!(space_schema.is_valid_relation("admin"));
    assert!(space_schema.is_valid_relation("member"));
    assert!(!space_schema.is_valid_relation("invalid"));

    // Check permissions
    assert!(space_schema.get_permission("delete").is_some());
    assert!(space_schema.get_permission("view").is_some());
    assert!(space_schema.get_permission("invalid").is_none());
}

#[test]
fn test_channel_schema() {
    let schema = PermissionSchema::default();
    let channel_schema = schema.get_schema(ObjectType::Channel).unwrap();

    // Check relations
    assert!(channel_schema.is_valid_relation("parent"));
    assert!(channel_schema.is_valid_relation("viewer"));

    // Check permission computation
    let delete_perm = channel_schema.get_permission("delete").unwrap();
    match delete_perm {
        ComputedPermission::Arrow(rel, perm) => {
            assert_eq!(rel, "parent");
            assert_eq!(perm, "admin");
        }
        _ => panic!("Expected Arrow permission"),
    }
}

#[test]
fn test_message_schema() {
    let schema = PermissionSchema::default();
    let message_schema = schema.get_schema(ObjectType::Message).unwrap();

    // Author can edit
    let edit_perm = message_schema.get_permission("edit").unwrap();
    match edit_perm {
        ComputedPermission::DirectRelation(rel) => {
            assert_eq!(rel, "author");
        }
        _ => panic!("Expected DirectRelation permission"),
    }
}

#[test]
fn spicedb_schema_uses_valid_direct_message_namespace() {
    assert!(SPICEDB_SCHEMA.contains("definition direct_message {"));
    assert!(!SPICEDB_SCHEMA.contains("definition dm {"));
}

#[test]
fn spicedb_schema_does_not_reuse_relation_names_as_permissions() {
    let mut current_definition = None;
    let mut relations = std::collections::HashSet::new();
    let mut permissions = std::collections::HashSet::new();

    for line in SPICEDB_SCHEMA.lines().map(str::trim) {
        if let Some(definition) = line
            .strip_prefix("definition ")
            .and_then(|line| line.strip_suffix(" {"))
        {
            current_definition = Some(definition);
            relations.clear();
            permissions.clear();
            continue;
        }

        if line == "}" {
            if let Some(definition) = current_definition.take() {
                let duplicates = relations
                    .intersection(&permissions)
                    .copied()
                    .collect::<Vec<_>>();
                assert!(
                    duplicates.is_empty(),
                    "definition {definition} reuses relation names as permissions: {duplicates:?}"
                );
            }
            continue;
        }

        if let Some(name) = line
            .strip_prefix("relation ")
            .and_then(|line| line.split(':').next())
        {
            relations.insert(name.trim());
        }

        if let Some(name) = line
            .strip_prefix("permission ")
            .and_then(|line| line.split('=').next())
        {
            permissions.insert(name.trim());
        }
    }
}
