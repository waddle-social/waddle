use anyhow::{ensure, Context, Result};
use sqlx::{Connection, PgConnection, Postgres, Row, Transaction};
use uuid::Uuid;

use super::model::{Candidate, Evidence, Fact, Identity, Intent, LeafRelation, Polarity};

pub fn matches(row: &sqlx::postgres::PgRow, identity: &Identity) -> Result<bool> {
    Ok(
        row.try_get::<String, _>("candidate")? == identity.candidate.storage()
            && row.try_get::<Uuid, _>("scope")? == identity.scope
            && row.try_get::<Uuid, _>("incarnation")? == identity.incarnation
            && row.try_get::<i64, _>("generation")? == identity.generation
            && row.try_get::<Uuid, _>("operation")? == identity.operation
            && row.try_get::<String, _>("digest")? == identity.digest_hex(),
    )
}

pub async fn fact(connection: &mut PgConnection, identity: &Identity, fact: Fact) -> Result<()> {
    sqlx::query("INSERT INTO recovery_fact(candidate,scope,incarnation,generation,operation,digest,fact) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
        .bind(identity.candidate.storage()).bind(identity.scope).bind(identity.incarnation).bind(identity.generation).bind(identity.operation).bind(identity.digest_hex()).bind(fact.storage()).execute(connection).await?;
    Ok(())
}

pub async fn prepare<'a>(
    connection: &'a mut PgConnection,
    identity: &Identity,
    intent: &Intent,
    previous: Option<&Identity>,
) -> Result<Transaction<'a, Postgres>> {
    identity.validate(intent)?;
    let mut tx = connection.begin().await?;
    let current =
        sqlx::query("SELECT * FROM recovery_guard WHERE candidate=$1 AND scope=$2 FOR UPDATE")
            .bind(identity.candidate.storage())
            .bind(identity.scope)
            .fetch_optional(&mut *tx)
            .await?;
    match previous {
        Some(previous) => {
            ensure!(
                identity.candidate == previous.candidate
                    && identity.scope == previous.scope
                    && identity.incarnation == previous.incarnation
                    && identity.generation
                        == previous
                            .generation
                            .checked_add(1)
                            .context("generation overflow")?
                    && identity.operation != previous.operation,
                "mutation must advance exact previous identity"
            );
            ensure!(
                current
                    .as_ref()
                    .map(|row| matches(row, previous))
                    .transpose()?
                    .unwrap_or(false),
                "previous recovery identity is no longer current"
            );
            sqlx::query("UPDATE recovery_guard SET incarnation=$3,generation=$4,operation=$5,digest=$6,active=false WHERE candidate=$1 AND scope=$2")
                .bind(identity.candidate.storage()).bind(identity.scope).bind(identity.incarnation).bind(identity.generation).bind(identity.operation).bind(identity.digest_hex()).execute(&mut *tx).await?;
        }
        None => {
            ensure!(
                current.is_none() && identity.generation == 1,
                "new recovery scope must be absent"
            );
            sqlx::query("INSERT INTO recovery_guard(candidate,scope,incarnation,generation,operation,digest,active) VALUES($1,$2,$3,$4,$5,$6,false)")
                .bind(identity.candidate.storage()).bind(identity.scope).bind(identity.incarnation).bind(identity.generation).bind(identity.operation).bind(identity.digest_hex()).execute(&mut *tx).await?;
        }
    }
    fact(&mut tx, identity, Fact::AdmissionClosed).await?;
    if identity.candidate == Candidate::Postgres {
        sqlx::query("INSERT INTO recovery_intent(candidate,scope,incarnation,generation,operation,digest,subject,relation,polarity) VALUES($1,$2,$3,$4,$5,$6,$7,'reader',$8)")
            .bind(identity.candidate.storage()).bind(identity.scope).bind(identity.incarnation).bind(identity.generation).bind(identity.operation).bind(identity.digest_hex()).bind(intent.subject).bind(intent.polarity.storage()).execute(&mut *tx).await?;
        fact(&mut tx, identity, Fact::CanonicalMutationCommitted).await?;
    }
    Ok(tx)
}

pub async fn current(
    connection: &mut PgConnection,
    identity: &Identity,
    active: bool,
) -> Result<bool> {
    let row = sqlx::query("SELECT * FROM recovery_guard WHERE candidate=$1 AND scope=$2")
        .bind(identity.candidate.storage())
        .bind(identity.scope)
        .fetch_optional(connection)
        .await?;
    Ok(row
        .as_ref()
        .map(|row| -> Result<bool> {
            Ok(matches(row, identity)? && row.try_get::<bool, _>("active")? == active)
        })
        .transpose()?
        .unwrap_or(false))
}

pub async fn canonical_intent(
    connection: &mut PgConnection,
    identity: &Identity,
) -> Result<Intent> {
    ensure!(
        identity.candidate == Candidate::Postgres,
        "SpiceDB recovery cannot read SQL grant authority"
    );
    let row = sqlx::query("SELECT * FROM recovery_intent WHERE candidate=$1 AND scope=$2 AND incarnation=$3 AND generation=$4 AND operation=$5 AND digest=$6")
        .bind(identity.candidate.storage()).bind(identity.scope).bind(identity.incarnation).bind(identity.generation).bind(identity.operation).bind(identity.digest_hex()).fetch_one(connection).await?;
    ensure!(
        row.try_get::<String, _>("relation")? == "reader",
        "unknown canonical leaf relation"
    );
    let polarity = match row.try_get::<String, _>("polarity")?.as_str() {
        "grant" => Polarity::Grant,
        "revoke" => Polarity::Revoke,
        _ => anyhow::bail!("unknown canonical polarity"),
    };
    let intent = Intent {
        subject: row.try_get("subject")?,
        relation: LeafRelation::Reader,
        polarity,
    };
    identity.validate(&intent)?;
    Ok(intent)
}

pub async fn activate(connection: &mut PgConnection, identity: &Identity) -> Result<bool> {
    let changed = sqlx::query("UPDATE recovery_guard SET active=true WHERE candidate=$1 AND scope=$2 AND incarnation=$3 AND generation=$4 AND operation=$5 AND digest=$6 AND active=false")
        .bind(identity.candidate.storage()).bind(identity.scope).bind(identity.incarnation).bind(identity.generation).bind(identity.operation).bind(identity.digest_hex()).execute(connection).await?;
    Ok(changed.rows_affected() == 1)
}

pub async fn evidence(connection: &mut PgConnection, identity: &Identity) -> Result<Vec<Evidence>> {
    let rows = sqlx::query("SELECT fact FROM recovery_fact WHERE candidate=$1 AND scope=$2 AND incarnation=$3 AND generation=$4 AND operation=$5 AND digest=$6 ORDER BY fact")
        .bind(identity.candidate.storage()).bind(identity.scope).bind(identity.incarnation).bind(identity.generation).bind(identity.operation).bind(identity.digest_hex()).fetch_all(connection).await?;
    rows.iter()
        .map(|row| {
            let fact = match row.try_get::<String, _>("fact")?.as_str() {
                "admission_closed" => Fact::AdmissionClosed,
                "canonical_committed" => Fact::CanonicalMutationCommitted,
                "projection_verified" => Fact::ProjectionVerified,
                _ => anyhow::bail!("unknown recovery evidence fact"),
            };
            Ok(Evidence {
                identity: identity.clone(),
                fact,
            })
        })
        .collect()
}
