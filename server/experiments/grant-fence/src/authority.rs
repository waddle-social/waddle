use anyhow::{ensure, Result};
use prescience::{
    Client, Consistency, ObjectReference, PermissionResult, Relationship, RelationshipFilter,
    RelationshipUpdate, SubjectReference,
};
use sqlx::{Connection, PgConnection, Row};
use uuid::Uuid;

use crate::model::{Guard, Settings, RPC_BUDGET};

pub async fn client(settings: &Settings) -> Result<Client> {
    client_at_port(settings, settings.grpc_port, RPC_BUDGET).await
}

pub async fn client_at_port(
    settings: &Settings,
    port: u16,
    timeout: std::time::Duration,
) -> Result<Client> {
    settings.validate()?;
    Ok(
        Client::builder(format!("http://127.0.0.1:{port}"), &settings.preshared_key)
            .connect_timeout(RPC_BUDGET)
            .default_timeout(timeout)
            .build()
            .await?,
    )
}

fn resource(guard: &Guard) -> Result<ObjectReference> {
    Ok(ObjectReference::new("fence_generation", guard.graph_id())?)
}

fn subject() -> Result<SubjectReference> {
    Ok(SubjectReference::new(
        ObjectReference::new("fence_user", "v1_alice")?,
        None::<String>,
    )?)
}

pub async fn check(client: &Client, guard: &Guard) -> Result<bool> {
    Ok(matches!(
        client
            .check_permission(&resource(guard)?, "read", &subject()?)
            .consistency(Consistency::FullyConsistent)
            .await?,
        PermissionResult::Allowed
    ))
}

pub async fn publish(client: &Client, guard: &Guard) -> Result<()> {
    client
        .write_relationships(vec![RelationshipUpdate::touch(Relationship::new(
            resource(guard)?,
            "reader",
            subject()?,
        ))])
        .await?;
    Ok(())
}

pub async fn delete_projection(client: &Client, guard: &Guard) -> Result<()> {
    client
        .delete_relationships(
            RelationshipFilter::new("fence_generation").resource_id(guard.graph_id()),
        )
        .await?;
    Ok(())
}

pub async fn prepare(connection: &mut PgConnection, guard: &Guard, recreate: bool) -> Result<()> {
    let mut tx = connection.begin().await?;
    if recreate {
        sqlx::query("DELETE FROM scope_guard WHERE scope=$1 AND active=false")
            .bind(guard.scope)
            .execute(&mut *tx)
            .await?;
    }
    let changed = sqlx::query("INSERT INTO scope_guard(scope,incarnation,generation,operation,graph_digest,desired,active) VALUES($1,$2,$3,$4,$5,true,false) ON CONFLICT(scope) DO UPDATE SET incarnation=EXCLUDED.incarnation,generation=EXCLUDED.generation,operation=EXCLUDED.operation,graph_digest=EXCLUDED.graph_digest,desired=true,active=false WHERE scope_guard.incarnation=EXCLUDED.incarnation AND scope_guard.generation<EXCLUDED.generation AND scope_guard.active=false")
        .bind(guard.scope).bind(guard.incarnation).bind(guard.generation).bind(guard.operation).bind(guard.digest()).execute(&mut *tx).await?;
    ensure!(
        changed.rows_affected() == 1,
        "canonical prepare must advance a closed generation"
    );
    tx.commit().await?;
    Ok(())
}

pub async fn activate(connection: &mut PgConnection, guard: &Guard) -> Result<bool> {
    let changed = sqlx::query("UPDATE scope_guard SET active=true WHERE scope=$1 AND incarnation=$2 AND generation=$3 AND operation=$4 AND graph_digest=$5 AND desired=true AND active=false")
        .bind(guard.scope).bind(guard.incarnation).bind(guard.generation).bind(guard.operation).bind(guard.digest()).execute(connection).await?;
    Ok(changed.rows_affected() == 1)
}

pub fn guard_matches(row: &sqlx::postgres::PgRow, guard: &Guard) -> Result<bool> {
    Ok(row.try_get::<Uuid, _>("incarnation")? == guard.incarnation
        && row.try_get::<i64, _>("generation")? == guard.generation
        && row.try_get::<Uuid, _>("operation")? == guard.operation
        && row.try_get::<String, _>("graph_digest")? == guard.digest()
        && row.try_get::<bool, _>("desired")?
        && row.try_get::<bool, _>("active")?)
}

pub async fn grant(settings: &Settings, guard: &Guard, recreate: bool) -> Result<()> {
    let mut connection = settings
        .connect("fence_app", "fence_app", "prepare", 3000)
        .await?;
    prepare(&mut connection, guard, recreate).await?;
    publish(&client(settings).await?, guard).await?;
    ensure!(
        activate(&mut connection, guard).await?,
        "fresh projection activation lost its exact operation"
    );
    Ok(())
}
