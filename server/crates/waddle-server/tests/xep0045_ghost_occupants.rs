//! XEP-0045 "Ghost Users": who may remove an occupant, and who may not.
//!
//! The service removes an occupant on a delivery-related error and then treats
//! it as having sent unavailable presence. That removal is the room's act: it
//! carries the §7.14 unavailable broadcast to the remaining occupants. A node
//! recovering a stalled groupchat obligation without the room's cleanup sweep
//! therefore has no eviction to make, and must keep owing the copy.
#[path = "ingress_cases/detached_progress_support.rs"]
pub mod detached_progress_support;
mod ingress_support;
#[path = "ingress_cases/muc_progress_support.rs"]
pub mod muc_progress_support;
use ingress_support::IngressFixture;

#[tokio::test]
async fn xep0045_seated_ghost_occupant_is_never_evicted_sqlite() {
    muc_progress_support::seated_ghost_occupant_is_never_evicted(IngressFixture::sqlite().await)
        .await;
}

#[tokio::test]
async fn xep0045_seated_ghost_occupant_is_never_evicted_postgres() {
    if let Some(fixture) = IngressFixture::postgres("xep0045_ghost_seated").await {
        muc_progress_support::seated_ghost_occupant_is_never_evicted(fixture).await;
    }
}
