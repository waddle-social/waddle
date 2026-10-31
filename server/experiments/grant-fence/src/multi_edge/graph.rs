use anyhow::{ensure, Context, Result};
use prescience::{
    Client, Consistency, ObjectReference, PermissionResult, Relationship, RelationshipFilter,
    RelationshipUpdate, SubjectReference, ZedToken,
};
use sha2::{Digest, Sha256};
use tokio_stream::StreamExt;
use uuid::Uuid;

use super::model::{Branch, Guard, Kind, Payload, Witness};
use crate::model::RPC_BUDGET;

fn object(kind: &str, id: Uuid) -> Result<ObjectReference> {
    Ok(ObjectReference::new(kind, id.simple().to_string())?)
}
fn relation(resource: ObjectReference, name: &str, target: ObjectReference) -> Relationship {
    Relationship::new(
        resource,
        name,
        SubjectReference::new(target, None::<String>).expect("fixed subject relation"),
    )
}
fn marker(guard: &Guard) -> Result<Relationship> {
    Ok(relation(
        ObjectReference::new("multi_marker", guard.marker_id())?,
        "version",
        ObjectReference::new(
            "multi_stamp",
            hex::encode(Sha256::digest(serde_json::to_vec(guard)?)),
        )?,
    ))
}
fn edge(guard: &Guard, principal: Uuid) -> Result<Option<Relationship>> {
    Ok(match guard.payload {
        Payload::Lifecycle(_) | Payload::Ancestor(false) | Payload::Direct(false) => None,
        Payload::Binding(parent) => Some(relation(
            object("multi_channel", guard.id)?,
            "parent",
            object("multi_parent", parent)?,
        )),
        Payload::Ancestor(true) => Some(relation(
            object("multi_parent", guard.id)?,
            "member",
            object("multi_user", principal)?,
        )),
        Payload::Direct(true) => Some(relation(
            object("multi_channel", guard.id)?,
            "direct",
            object("multi_user", principal)?,
        )),
    })
}
pub async fn publish(
    client: &Client,
    previous: Option<&Guard>,
    next: &Guard,
    principal: Uuid,
) -> Result<()> {
    let mut changes = Vec::new();
    let next_edge = edge(next, principal)?;
    if let Some(previous) = previous {
        changes.push(RelationshipUpdate::delete(marker(previous)?));
        if let Some(old) = edge(previous, principal)?.filter(|old| next_edge.as_ref() != Some(old))
        {
            changes.push(RelationshipUpdate::delete(old));
        }
    }
    changes.push(RelationshipUpdate::touch(marker(next)?));
    if let Some(new) = edge(next, principal)? {
        changes.push(RelationshipUpdate::touch(new));
    }
    // Version marker and edge change are one atomic actual SpiceDB write.
    tokio::time::timeout(RPC_BUDGET, client.write_relationships(changes)).await??;
    Ok(())
}
async fn rows(
    client: &Client,
    resource: &ObjectReference,
    relation_name: &str,
    consistency: Consistency,
) -> Result<Vec<(Relationship, ZedToken)>> {
    let mut stream = Box::pin(
        client
            .read_relationships(
                RelationshipFilter::new(resource.object_type())
                    .resource_id(resource.object_id())
                    .relation(relation_name),
            )
            .consistency(consistency)
            .limit(5)
            .send()
            .await?,
    );
    let mut result = Vec::new();
    while let Some(row) = stream.next().await {
        let row = row?;
        ensure!(result.len() < 4, "bounded graph relationship overflow");
        result.push((row.relationship, row.read_at));
    }
    Ok(result)
}
async fn exact(
    client: &Client,
    expected: Option<Relationship>,
    resource: &ObjectReference,
    relation_name: &str,
    revision: &ZedToken,
) -> Result<bool> {
    let found = rows(
        client,
        resource,
        relation_name,
        Consistency::AtExactSnapshot(revision.clone()),
    )
    .await?;
    ensure!(
        found.iter().all(|(_, token)| token == revision),
        "snapshot revision mismatch"
    );
    Ok(match expected {
        Some(expected) => found.len() == 1 && found[0].0 == expected,
        None => found.is_empty(),
    })
}
pub async fn anchor(client: &Client, guard: &Guard) -> Result<Option<ZedToken>> {
    let expected = marker(guard)?;
    let found = rows(
        client,
        &expected.resource,
        "version",
        Consistency::FullyConsistent,
    )
    .await?;
    Ok(if found.len() == 1 && found[0].0 == expected {
        Some(found[0].1.clone())
    } else {
        None
    })
}
async fn verify_guard(
    client: &Client,
    guard: &Guard,
    principal: Uuid,
    revision: &ZedToken,
) -> Result<bool> {
    let expected = marker(guard)?;
    if !exact(
        client,
        Some(expected.clone()),
        &expected.resource,
        "version",
        revision,
    )
    .await?
    {
        return Ok(false);
    }
    let (resource, name) = match guard.payload {
        Payload::Lifecycle(_) => return Ok(true),
        Payload::Binding(_) => (object("multi_channel", guard.id)?, "parent"),
        Payload::Ancestor(_) => (object("multi_parent", guard.id)?, "member"),
        Payload::Direct(_) => (object("multi_channel", guard.id)?, "direct"),
    };
    exact(client, edge(guard, principal)?, &resource, name, revision).await
}
pub async fn verify_mutation(client: &Client, guard: &Guard, principal: Uuid) -> Result<bool> {
    tokio::time::timeout(RPC_BUDGET, async {
        let Some(revision) = anchor(client, guard).await? else {
            return Ok(false);
        };
        verify_guard(client, guard, principal, &revision).await
    })
    .await
    .context("mutation graph deadline")?
}
pub async fn witness(client: &Client, witness: &Witness, revision: ZedToken) -> Result<bool> {
    tokio::time::timeout(RPC_BUDGET, async {
        validate(witness)?;
        if !guards_match(client, witness, &revision).await? {
            return Ok(false);
        }
        let permission = match witness.branch {
            Branch::Inherited => "inherited",
            Branch::Direct => "direct_access",
        };
        Ok(matches!(
            client
                .check_permission(
                    &object("multi_channel", witness.channel)?,
                    permission,
                    &SubjectReference::new(
                        object("multi_user", witness.principal)?,
                        None::<String>
                    )?
                )
                .consistency(Consistency::AtExactSnapshot(revision))
                .await?,
            PermissionResult::Allowed
        ))
    })
    .await
    .context("witness graph deadline")?
}
/// Verify every marker and edge at the same revision before checking permission.
pub async fn guards_match(client: &Client, witness: &Witness, revision: &ZedToken) -> Result<bool> {
    validate(witness)?;
    for guard in &witness.guards {
        if !verify_guard(client, guard, witness.principal, revision).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Fixture-only denial overlay for the selected-branch negative control.
pub async fn deny_inherited_control(client: &Client, channel: Uuid, principal: Uuid) -> Result<()> {
    let denied = relation(
        object("multi_channel", channel)?,
        "control_denied",
        object("multi_user", principal)?,
    );
    tokio::time::timeout(
        RPC_BUDGET,
        client.write_relationships(vec![RelationshipUpdate::touch(denied)]),
    )
    .await??;
    Ok(())
}

/// Fixture-only corruption makes a graph read fail instead of returning denial.
pub async fn overflow_marker_control(client: &Client, guard: &Guard) -> Result<()> {
    let marker = marker(guard)?;
    let mut updates = Vec::new();
    for _ in 0..4 {
        updates.push(RelationshipUpdate::touch(relation(
            marker.resource.clone(),
            "version",
            object("multi_stamp", Uuid::new_v4())?,
        )));
    }
    tokio::time::timeout(RPC_BUDGET, client.write_relationships(updates)).await??;
    Ok(())
}

pub fn validate(witness: &Witness) -> Result<()> {
    let expected: &[Kind] = match witness.branch {
        Branch::Inherited => &[Kind::Lifecycle, Kind::Binding, Kind::Ancestor],
        Branch::Direct => &[Kind::Lifecycle, Kind::Direct],
    };
    ensure!(
        witness
            .guards
            .iter()
            .map(Guard::kind)
            .eq(expected.iter().copied()),
        "incomplete or unordered dependency vector"
    );
    ensure!(
        witness.guards[0].id == witness.channel,
        "lifecycle must be current channel row"
    );
    match witness.branch {
        Branch::Inherited => {
            ensure!(
                witness.guards[1].id == witness.channel
                    && witness.guards[1].payload == Payload::Binding(witness.guards[2].id)
                    && witness.guards[2].payload == Payload::Ancestor(true),
                "inherited dependency binding mismatch"
            );
        }
        Branch::Direct => ensure!(
            witness.guards[1].id == witness.channel
                && witness.guards[1].payload == Payload::Direct(true),
            "direct dependency mismatch"
        ),
    }
    Ok(())
}

/// Independent current-policy probe for negative-control fixture assertions.
/// Both branch checks share a fresh positive marker's actual revision.
pub async fn current_permissions(
    client: &Client,
    lifecycle: &Guard,
    principal: Uuid,
) -> Result<(bool, bool)> {
    tokio::time::timeout(RPC_BUDGET, async {
        let revision = anchor(client, lifecycle)
            .await?
            .context("current policy anchor missing")?;
        let resource = object("multi_channel", lifecycle.id)?;
        let subject = SubjectReference::new(object("multi_user", principal)?, None::<String>)?;
        let inherited = client
            .check_permission(&resource, "inherited", &subject)
            .consistency(Consistency::AtExactSnapshot(revision.clone()))
            .await?;
        let direct = client
            .check_permission(&resource, "direct_access", &subject)
            .consistency(Consistency::AtExactSnapshot(revision))
            .await?;
        Ok((
            matches!(inherited, PermissionResult::Allowed),
            matches!(direct, PermissionResult::Allowed),
        ))
    })
    .await
    .context("current branch probe deadline")?
}
