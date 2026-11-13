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

/// The migrations after the baseline, in version order.
pub fn all() -> Vec<Migration> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::super::BASELINE_VERSION;
    use super::all;

    #[test]
    fn later_migrations_follow_the_baseline_in_order() {
        let versions: Vec<i64> = all().iter().map(|migration| migration.version).collect();
        assert!(versions.iter().all(|version| *version > BASELINE_VERSION));
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
