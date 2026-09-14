//! XEP-0421 occupant identifiers survive replay and maintenance reconstruction.
#[path = "ingress_cases/detached_progress_support.rs"]
pub mod detached_progress_support;
mod ingress_support;
#[path = "ingress_cases/muc_progress_support.rs"]
pub mod muc_progress_support;
use ingress_support::IngressFixture;

macro_rules! occupant_case {
    ($sqlite:ident, $postgres:ident, $case:ident $(, $arg:expr)*) => {
        #[tokio::test]
        async fn $sqlite() {
            muc_progress_support::$case(IngressFixture::sqlite().await $(, $arg)*).await;
        }
        #[tokio::test]
        async fn $postgres() {
            if let Some(fixture) = IngressFixture::postgres("xep0421_copies").await {
                muc_progress_support::$case(fixture $(, $arg)*).await;
            }
        }
    };
}

occupant_case!(
    xep0421_replay_preserves_frozen_occupant_id_sqlite,
    xep0421_replay_preserves_frozen_occupant_id_postgres,
    occupant_replay,
    false
);
occupant_case!(
    xep0421_maintenance_preserves_frozen_occupant_id_sqlite,
    xep0421_maintenance_preserves_frozen_occupant_id_postgres,
    occupant_maintenance
);
occupant_case!(
    xep0421_system_replay_preserves_frozen_occupant_id_sqlite,
    xep0421_system_replay_preserves_frozen_occupant_id_postgres,
    occupant_replay,
    true
);
occupant_case!(
    xep0421_missing_occupant_id_prevents_rebuild_sqlite,
    xep0421_missing_occupant_id_prevents_rebuild_postgres,
    missing_occupant_replay
);
