//! A room an enrolled host-grant scope manages, hosted for occupants at
//! another server: the revocation fixture's deployment on PostgreSQL, one
//! replica that runs an S2S snapshot admitting `home-b.test`, and the
//! inbound MUC wiring over that replica's state.
//!
//! Suites that include this module also declare `host_grant_support`,
//! `host_grant_revocation_support` and `ingress_support`.

use std::sync::Arc;

use jid::BareJid;
use waddle_server::permissions::host_grant::RoomAffiliation;
use waddle_server::s2s::route::RouterLimits;

use crate::host_grant_revocation_support::Deployment;
use crate::host_grant_support::OWNER;
use crate::s2s_inbound_muc_support::{Hosted, Spec, Wiring};

/// The account at `home-b.test` the cases give a membership.
pub const GUEST: &str = "guest@home-b.test";
/// Its resource.
pub const GUEST_PHONE: &str = "guest@home-b.test/phone";
/// An account at `home-b.test` the room's grants do not name.
pub const STRANGER_PHONE: &str = "mallory@home-b.test/phone";

/// The managed room's deployment and its S2S side.
pub struct Managed {
    pub deployment: Deployment,
    pub hosted: Hosted,
}

impl Managed {
    /// The fixture's scope on one replica that hosts its room for
    /// `home-b.test`, with [`GUEST`] an effective member of the room.
    /// `None` without PostgreSQL.
    pub async fn start(label: &str, adjust: impl FnOnce(&mut Spec)) -> Option<Self> {
        let wiring = Wiring::new(adjust).await;
        let deployment = Deployment::start_running(label, 1, &wiring.config).await?;
        deployment
            .grants
            .set(OWNER, GUEST, RoomAffiliation::Member)
            .await
            .expect("the remote membership");
        deployment
            .grants
            .activate_current(&deployment.grants.scope)
            .await;
        let state = Arc::clone(&deployment.replicas[0].state);
        let hosted = Hosted::serve(wiring, state, RouterLimits::default()).await;
        Some(Self { deployment, hosted })
    }

    pub fn room(&self) -> BareJid {
        self.deployment.room().clone()
    }

    /// The occupant JID of `nick` in the managed room.
    pub fn occupant(&self, nick: &str) -> String {
        [self.deployment.room().as_str(), "/", nick].concat()
    }
}
