//! `owner-phase`: prepare every schema the restricted runtime attaches to.

use uuid::Uuid;

#[cfg(feature = "clustering")]
use crate::clustering::lease::{configured_peer_order, KeypoolOwnerError};
use crate::config::{session_key_from_env, ClusteringConfig, LineageConfig};
use crate::db::runtime_startup::database_identity_json;
use crate::db::{Database, DatabaseError};
use crate::server::runtime_owner::{
    prepare_restricted_runtime_owner, RestrictedRuntimeOwnerError, RestrictedRuntimeOwnerPlan,
};
#[cfg(feature = "clustering")]
use crate::server::runtime_owner::{
    AllowlistRemoval, ClusteringOwnerPlan, ExclusiveDatabase, LegacyKeypairSlots,
};

use super::args::OwnerPhaseArgs;
#[cfg(feature = "clustering")]
use super::args::{AllowlistArg, ClusteringArgs, ExclusiveDatabaseArg, LegacyKeypairSlotsArg};
use super::connection::open_owner;
use super::error::OwnerCliError;

pub(super) async fn run(args: OwnerPhaseArgs) -> Result<(), OwnerCliError> {
    let lineage = LineageConfig::from_env()
        .map_err(|error| OwnerCliError::Configuration(error.to_string()))?;
    if lineage.deployment_uuid.is_none() {
        return Err(OwnerCliError::Configuration(
            "WADDLE_DEPLOYMENT_UUID is required".to_owned(),
        ));
    }
    let session_key = session_key_from_env().map_err(OwnerCliError::Configuration)?;
    let clustering = ClusteringConfig::from_env().map_err(OwnerCliError::Configuration)?;
    // Generated per run: with it a clustered phase tells its own sessions
    // from every other client's, including another session of this login.
    let application_name = format!("waddle-owner-{}", Uuid::new_v4().simple());
    let plan = RestrictedRuntimeOwnerPlan {
        lineage,
        migrator: args.connection.migrator_login.clone(),
        schema_owner: args.connection.schema_owner.clone(),
        runtime: args.roles.runtime_login.clone(),
        attestation_definer: args.roles.attestor_definer_role.clone(),
        vapid_root_key: session_key.into_bytes(),
        #[cfg(feature = "clustering")]
        clustering: clustering_plan(&clustering, &args.clustering, &application_name)?,
    };
    #[cfg(not(feature = "clustering"))]
    if clustering.enabled {
        return Err(OwnerCliError::Configuration(
            "WADDLE_CLUSTERING_ENABLED is true, but this binary was built without clustering"
                .to_owned(),
        ));
    }

    let owner = open_owner(&args.connection, &application_name).await?;
    let evidence = match prepare_restricted_runtime_owner(&owner, &plan).await {
        Ok(evidence) => evidence,
        Err(RestrictedRuntimeOwnerError::KeypoolActivationPending) => {
            return Err(OwnerCliError::KeypoolActivationPending {
                activation: pending_activation(&owner).await,
            });
        }
        Err(error) => {
            if not_exclusive(&error) {
                report_other_sessions(&owner, &application_name).await;
            }
            return Err(OwnerCliError::OwnerPhase(error));
        }
    };

    let identity =
        database_identity_json(evidence.attested_lineage()).map_err(OwnerCliError::Handoff)?;
    if let Some(path) = &args.identity_output {
        std::fs::write(path, &identity).map_err(|source| OwnerCliError::Write {
            path: path.clone(),
            source,
        })?;
    }
    println!("{identity}");
    eprintln!("owner phase complete: every schema is prepared and the runtime role is granted");
    Ok(())
}

/// The clustering section of the plan, present when the server is
/// configured to cluster. PeerIds come from the server's own keypair pool,
/// so the enrollment cannot name a pool the servers do not hold.
#[cfg(feature = "clustering")]
fn clustering_plan(
    config: &ClusteringConfig,
    args: &ClusteringArgs,
    application_name: &str,
) -> Result<Option<ClusteringOwnerPlan>, OwnerCliError> {
    if !config.enabled {
        return Ok(None);
    }
    let configuration = |message: &str| OwnerCliError::Configuration(message.to_owned());
    if config.keypair_pool.is_empty() {
        return Err(configuration(
            "WADDLE_CLUSTERING_KEYPAIR_POOL is required when WADDLE_CLUSTERING_ENABLED is true",
        ));
    }
    let keypool_generation = args.keypool_generation.ok_or_else(|| {
        configuration(
            "--keypool-generation (WADDLE_CLUSTERING_KEYPOOL_GENERATION) is required when \
             WADDLE_CLUSTERING_ENABLED is true",
        )
    })?;
    // The same derivation the runtime attachment compares with the
    // enrollment. The entries are secrets, so the error quotes none.
    let peer_ids = configured_peer_order(&config.keypair_pool).map_err(|_| {
        configuration(
            "WADDLE_CLUSTERING_KEYPAIR_POOL must hold distinct base64-encoded 32-byte ed25519 \
             secrets",
        )
    })?;
    let legacy_keypair_slots = match (args.legacy_keypair_slots, args.legacy_ttl) {
        (LegacyKeypairSlotsArg::Refuse, None) => LegacyKeypairSlots::Refuse,
        (LegacyKeypairSlotsArg::Refuse, Some(_)) => {
            return Err(configuration(
                "--legacy-ttl applies only with --legacy-keypair-slots retire",
            ));
        }
        (LegacyKeypairSlotsArg::Retire, Some(legacy_ttl)) => {
            LegacyKeypairSlots::Retire { legacy_ttl }
        }
        (LegacyKeypairSlotsArg::Retire, None) => {
            return Err(configuration(
                "--legacy-keypair-slots retire requires --legacy-ttl",
            ));
        }
    };
    let activation_delay = args
        .keypool_activation_delay
        .or(args.legacy_ttl)
        .unwrap_or_default();
    Ok(Some(ClusteringOwnerPlan {
        peer_ids,
        keypool_generation,
        activation_delay,
        legacy_keypair_slots,
        lease_ttl: config.lease.lease_ttl,
        allowlist: match args.allowlist {
            AllowlistArg::Refuse => AllowlistRemoval::Refuse,
            AllowlistArg::Reconcile => AllowlistRemoval::Reconcile,
        },
        exclusive_database: match args.exclusive_database {
            ExclusiveDatabaseArg::Required => ExclusiveDatabase::Required {
                owner_application_name: application_name.to_owned(),
            },
            ExclusiveDatabaseArg::NotChecked => ExclusiveDatabase::NotChecked,
        },
    }))
}

/// When the pending generation can be activated, on the database clock.
async fn pending_activation(owner: &Database) -> String {
    let read = async {
        let connection = owner.guard().await?;
        let mut rows = connection
            .query(
                "SELECT pg_catalog.to_char(activation_after AT TIME ZONE 'UTC', \
                          'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
                        GREATEST(0, pg_catalog.ceil(pg_catalog.date_part('epoch', \
                          activation_after - pg_catalog.now())))::bigint \
                   FROM clustering_keypool WHERE singleton",
                (),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok::<_, DatabaseError>(Some((row.get::<String>(0)?, row.get::<i64>(1)?))),
            None => Ok(None),
        }
    };
    match read.await {
        Ok(Some((at, seconds))) => format!(
            "run owner-phase again after {at} (in {seconds} s on the database clock), once no \
             keypair slot is leased; the server refuses to start until that run succeeds"
        ),
        Ok(None) | Err(_) => {
            "run owner-phase again after the activation delay; the activation time could not be \
             read"
                .to_owned()
        }
    }
}

fn not_exclusive(error: &RestrictedRuntimeOwnerError) -> bool {
    match error {
        RestrictedRuntimeOwnerError::DatabaseNotExclusive { .. } => true,
        #[cfg(feature = "clustering")]
        RestrictedRuntimeOwnerError::Keypool(KeypoolOwnerError::DatabaseNotExclusive {
            ..
        }) => true,
        _ => false,
    }
}

/// Name the sessions and prepared transactions that made the database
/// non-exclusive, as far as this login may see them.
async fn report_other_sessions(owner: &Database, application_name: &str) {
    let read = async {
        let connection = owner.guard().await?;
        let mut rows = connection
            .query(
                "SELECT 'session pid=' || activity.pid::text \
                        || ' login=' || activity.usename::text \
                        || ' application_name=' \
                        || pg_catalog.quote_literal(COALESCE(activity.application_name, '')) \
                        || ' client=' || COALESCE(pg_catalog.host(activity.client_addr), 'local') \
                        || ' state=' || COALESCE(activity.state, 'unknown') \
                   FROM pg_catalog.pg_stat_activity activity \
                  WHERE activity.datname = pg_catalog.current_database() \
                    AND activity.pid <> pg_catalog.pg_backend_pid() \
                    AND activity.usename IS NOT NULL \
                    AND (activity.backend_type IS NULL \
                      OR activity.backend_type = 'client backend') \
                    AND NOT (activity.usename = session_user \
                      AND activity.application_name = ?) \
                 UNION ALL \
                 SELECT 'prepared transaction gid=' || pg_catalog.quote_literal(prepared.gid) \
                        || ' owner=' || prepared.owner::text \
                   FROM pg_catalog.pg_prepared_xacts prepared \
                  WHERE prepared.database = pg_catalog.current_database()",
                crate::db_params![application_name],
            )
            .await?;
        let mut lines = Vec::new();
        while let Some(row) = rows.next().await? {
            lines.push(row.get::<String>(0)?);
        }
        Ok::<_, DatabaseError>(lines)
    };
    match read.await {
        Ok(lines) => {
            for line in lines {
                eprintln!("  other {line}");
            }
        }
        Err(error) => eprintln!("  the other sessions could not be listed: {error}"),
    }
}
