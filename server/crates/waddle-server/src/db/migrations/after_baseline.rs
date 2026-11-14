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

/// Version of the migration that creates the S2S IQ correlation table.
pub const S2S_IQ_CORRELATIONS_VERSION: i64 = 2003;

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
            version: S2S_IQ_CORRELATIONS_VERSION,
            description: "s2s iq correlations".to_string(),
            sql_sqlite: include_str!("../../../sql/migrations/2003_s2s_iq_correlations.sqlite.sql"),
            sql_postgres: include_str!(
                "../../../sql/migrations/2003_s2s_iq_correlations.postgres.sql"
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::super::BASELINE_VERSION;
    use super::{all, HOST_GRANT_AUTHORITY_VERSION, S2S_IQ_CORRELATIONS_VERSION};

    #[test]
    fn later_migrations_follow_the_baseline_in_order() {
        let versions: Vec<i64> = all().iter().map(|migration| migration.version).collect();
        assert!(versions.iter().all(|version| *version > BASELINE_VERSION));
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            versions,
            vec![HOST_GRANT_AUTHORITY_VERSION, S2S_IQ_CORRELATIONS_VERSION]
        );
    }
}
