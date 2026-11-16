//! Schema changes made after the schema baseline.
//!
//! The baseline (version 2000, [`super::baseline`]) builds the whole schema
//! as it stood when the migration history was retired. Every later change is
//! a migration here, numbered from 2001 upwards, with SQL for both engines,
//! and is applied after the baseline on a fresh database and on a stamped
//! one alike. The baseline itself is never edited: a changed checksum would
//! fail every database that recorded it.
//!
//! A change to the schema must keep the two engines in lockstep; see
//! `docs/planning/2046-schema-baseline.md`, "Keeping the engines in lockstep
//! after the cut".

use super::Migration;

/// Version of the migration that creates the host-grant authority tables.
pub const HOST_GRANT_AUTHORITY_VERSION: i64 = 2001;

/// Version of the migration that binds the host-grant authority incarnation
/// to the declared restore epoch and the database lineage.
pub const HOST_GRANT_RESTORE_INCARNATION_VERSION: i64 = 2002;

/// Version of the migration that creates the S2S IQ correlation table.
pub const S2S_IQ_CORRELATIONS_VERSION: i64 = 2003;

/// Version of the migration that creates the S2S remote room occupancy
/// table and its room verdict cache.
pub const S2S_REMOTE_ROOMS_VERSION: i64 = 2004;

/// Version of the migration that creates the host-grant revocation fence's
/// replica leases and acknowledgements.
pub const HOST_GRANT_REVOCATION_FENCE_VERSION: i64 = 2005;

/// Version of the migration that creates the ledger of retired account
/// JIDs.
pub const RETIRED_ACCOUNT_JIDS_VERSION: i64 = 2006;

/// Version of the migration that creates the table of the seats remote
/// occupants hold in local rooms.
pub const S2S_LOCAL_ROOM_SEATS_VERSION: i64 = 2007;

/// Version of the migration that adds the federated setting to the channel
/// catalog.
pub const CHANNEL_FEDERATED_VERSION: i64 = 2008;

/// The migrations after the baseline, in version order.
pub fn all() -> Vec<Migration> {
    vec![
        Migration {
            version: HOST_GRANT_AUTHORITY_VERSION,
            description: "host-grant authority".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2001_host_grant_authority.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2001_host_grant_authority.postgres.sql"
            ),
        },
        Migration {
            version: HOST_GRANT_RESTORE_INCARNATION_VERSION,
            description: "host-grant restore incarnation".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2002_host_grant_restore_incarnation.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2002_host_grant_restore_incarnation.postgres.sql"
            ),
        },
        Migration {
            version: S2S_IQ_CORRELATIONS_VERSION,
            description: "s2s iq correlations".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2003_s2s_iq_correlations.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2003_s2s_iq_correlations.postgres.sql"
            ),
        },
        Migration {
            version: S2S_REMOTE_ROOMS_VERSION,
            description: "s2s remote rooms".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2004_s2s_remote_rooms.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2004_s2s_remote_rooms.postgres.sql"
            ),
        },
        Migration {
            version: HOST_GRANT_REVOCATION_FENCE_VERSION,
            description: "host-grant revocation fence".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2005_host_grant_revocation_fence.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2005_host_grant_revocation_fence.postgres.sql"
            ),
        },
        Migration {
            version: RETIRED_ACCOUNT_JIDS_VERSION,
            description: "retired account jids".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2006_retired_account_jids.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2006_retired_account_jids.postgres.sql"
            ),
        },
        Migration {
            version: S2S_LOCAL_ROOM_SEATS_VERSION,
            description: "s2s local room seats".to_string(),
            sql_sqlite: include_str!(
                "../../../sql/migrations/2007_s2s_local_room_seats.sqlite.sql"
            ),
            sql_postgres: include_str!(
                "../../../sql/migrations/2007_s2s_local_room_seats.postgres.sql"
            ),
        },
        Migration {
            version: CHANNEL_FEDERATED_VERSION,
            description: "channel federated setting".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2008_channel_federated.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2008_channel_federated.postgres.sql"
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::super::BASELINE_VERSION;
    use super::{
        all, CHANNEL_FEDERATED_VERSION, HOST_GRANT_AUTHORITY_VERSION,
        HOST_GRANT_RESTORE_INCARNATION_VERSION, HOST_GRANT_REVOCATION_FENCE_VERSION,
        RETIRED_ACCOUNT_JIDS_VERSION, S2S_IQ_CORRELATIONS_VERSION, S2S_LOCAL_ROOM_SEATS_VERSION,
        S2S_REMOTE_ROOMS_VERSION,
    };

    #[test]
    fn later_migrations_follow_the_baseline_in_order() {
        let versions: Vec<i64> = all().iter().map(|migration| migration.version).collect();
        assert!(versions.iter().all(|version| *version > BASELINE_VERSION));
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            versions,
            vec![
                HOST_GRANT_AUTHORITY_VERSION,
                HOST_GRANT_RESTORE_INCARNATION_VERSION,
                S2S_IQ_CORRELATIONS_VERSION,
                S2S_REMOTE_ROOMS_VERSION,
                HOST_GRANT_REVOCATION_FENCE_VERSION,
                RETIRED_ACCOUNT_JIDS_VERSION,
                S2S_LOCAL_ROOM_SEATS_VERSION,
                CHANNEL_FEDERATED_VERSION
            ]
        );
    }
}
