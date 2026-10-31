use anyhow::{ensure, Context, Result};
use prescience::{
    Client, Consistency, ObjectReference, PermissionResult, Relationship, RelationshipFilter,
    RelationshipUpdate, SubjectReference, ZedToken,
};
use tokio_stream::StreamExt;
use uuid::Uuid;

use super::model::{Component, Identity, Intent, LeafRelation, Polarity};
use crate::model::RPC_BUDGET;

fn resource(identity: &Identity) -> Result<ObjectReference> {
    Ok(ObjectReference::new(
        "recovery_operation",
        identity.graph_id(),
    )?)
}
fn subject(id: &str) -> Result<SubjectReference> {
    Ok(SubjectReference::new(
        ObjectReference::new("recovery_subject", id)?,
        None::<String>,
    )?)
}

/// One acknowledged SpiceDB transaction commits the marker and positive leaf.
/// A revoke is a positive tombstone, never inferred from tuple absence.
pub async fn publish(client: &Client, identity: &Identity, intent: &Intent) -> Result<()> {
    identity.validate(intent)?;
    client
        .write_relationships(vec![
            RelationshipUpdate::touch(Relationship::new(
                resource(identity)?,
                "marker",
                subject("committed")?,
            )),
            RelationshipUpdate::touch(Relationship::new(
                resource(identity)?,
                intent.polarity.relation(),
                subject(&format!("u_{}", intent.subject.simple()))?,
            )),
        ])
        .await?;
    Ok(())
}

pub struct Snapshot {
    pub intent: Intent,
    revision: ZedToken,
}

/// Read the complete bounded object in one streaming RPC. All rows must name
/// one read revision; marker and leaf cannot come from separate observations.
pub async fn verify(client: &Client, identity: &Identity) -> Result<Option<Snapshot>> {
    tokio::time::timeout(RPC_BUDGET, async {
        let mut stream = Box::pin(
            client
                .read_relationships(
                    RelationshipFilter::new("recovery_operation").resource_id(identity.graph_id()),
                )
                .consistency(Consistency::FullyConsistent)
                .limit(4)
                .send()
                .await?,
        );
        let mut revision = None;
        let mut marker = false;
        let mut intent = None;
        let mut count = 0;
        while let Some(row) = stream.next().await {
            let row = row?;
            ensure!(
                revision
                    .as_ref()
                    .is_none_or(|revision| revision == &row.read_at),
                "canonical read mixed SpiceDB revisions"
            );
            revision = Some(row.read_at);
            let relationship = row.relationship;
            ensure!(
                relationship.resource == resource(identity)?
                    && relationship.optional_caveat.is_none()
                    && relationship.subject.optional_relation().is_none()
                    && relationship.subject.object().object_type() == "recovery_subject",
                "unexpected canonical relationship shape"
            );
            count += 1;
            match relationship.relation.as_str() {
                "marker" => {
                    ensure!(
                        !marker && relationship.subject.object().object_id() == "committed",
                        "canonical marker conflict"
                    );
                    marker = true;
                }
                "reader" | "tombstone" => {
                    ensure!(intent.is_none(), "multiple canonical leaves");
                    let id = relationship
                        .subject
                        .object()
                        .object_id()
                        .strip_prefix("u_")
                        .context("canonical subject version")?;
                    intent = Some(Intent {
                        subject: Uuid::parse_str(id)?,
                        relation: LeafRelation::Reader,
                        polarity: if relationship.relation == "reader" {
                            Polarity::Grant
                        } else {
                            Polarity::Revoke
                        },
                    });
                }
                _ => anyhow::bail!("unknown canonical relationship"),
            }
        }
        if !marker || intent.is_none() {
            return Ok(None);
        }
        ensure!(
            count == 2,
            "canonical object must contain exactly marker and leaf"
        );
        let intent = intent.context("canonical leaf")?;
        identity.validate(&intent)?;
        Ok(Some(Snapshot {
            intent,
            revision: revision.context("canonical revision")?,
        }))
    })
    .await
    .context("canonical verification deadline")?
}

pub async fn allowed(client: &Client, identity: &Identity) -> Result<bool> {
    let Some(snapshot) = verify(client, identity).await? else {
        return Ok(false);
    };
    if snapshot.intent.polarity != Polarity::Grant {
        return Ok(false);
    }
    Ok(matches!(
        client
            .check_permission(
                &resource(identity)?,
                "read",
                &subject(&format!("u_{}", snapshot.intent.subject.simple()))?
            )
            .consistency(Consistency::AtExactSnapshot(snapshot.revision))
            .await?,
        PermissionResult::Allowed
    ))
}

pub async fn remove(client: &Client, identity: &Identity, component: Component) -> Result<()> {
    client
        .delete_relationships(
            RelationshipFilter::new("recovery_operation")
                .resource_id(identity.graph_id())
                .relation(component.relation()),
        )
        .await?;
    Ok(())
}
