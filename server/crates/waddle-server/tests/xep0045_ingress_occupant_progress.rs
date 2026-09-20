//! XEP-0045 §7.4 rebuilt occupant copies retain the room-authored wire identity.
#[path = "ingress_cases/detached_progress_support.rs"]
pub mod detached_progress_support;
mod ingress_support;
#[path = "ingress_cases/muc_progress_support.rs"]
pub mod muc_progress_support;
use ingress_support::IngressFixture;

macro_rules! replay_case {
    ($sqlite:ident, $postgres:ident, $system:expr, $reconnect:expr, $old:expr) => {
        #[tokio::test]
        async fn $sqlite() {
            muc_progress_support::replay(IngressFixture::sqlite().await, $system, $reconnect, $old)
                .await;
        }
        #[tokio::test]
        async fn $postgres() {
            if let Some(fixture) = IngressFixture::postgres("xep0045_muc_wire").await {
                muc_progress_support::replay(fixture, $system, $reconnect, $old).await;
            }
        }
    };
}
replay_case!(
    xep0045_ingress_occupant_progress_replay_sqlite,
    xep0045_ingress_occupant_progress_replay_postgres,
    false,
    false,
    false
);
replay_case!(
    xep0045_ingress_occupant_progress_system_sqlite,
    xep0045_ingress_occupant_progress_system_postgres,
    true,
    false,
    false
);
replay_case!(
    xep0045_ingress_occupant_progress_rejoined_sender_sqlite,
    xep0045_ingress_occupant_progress_rejoined_sender_postgres,
    false,
    true,
    false
);
replay_case!(
    xep0045_ingress_occupant_progress_old_sender_sqlite,
    xep0045_ingress_occupant_progress_old_sender_postgres,
    false,
    false,
    true
);

#[tokio::test]
async fn xep0045_ingress_occupant_progress_departed_occupant_sqlite() {
    muc_progress_support::departed_occupant_maintenance(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn xep0045_ingress_occupant_progress_departed_occupant_postgres() {
    if let Some(fixture) = IngressFixture::postgres("xep0045_muc_departed").await {
        muc_progress_support::departed_occupant_maintenance(fixture).await;
    }
}

#[tokio::test]
async fn xep0045_ingress_occupant_progress_maintenance_sqlite() {
    muc_progress_support::maintenance(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn xep0045_ingress_occupant_progress_maintenance_postgres() {
    if let Some(fixture) = IngressFixture::postgres("xep0045_muc_maintenance").await {
        muc_progress_support::maintenance(fixture).await;
    }
}
