use crate::server::routes::interpret::OrderedRelayRouteOrigin;

#[cfg(test)]
tokio::task_local! {
    pub(crate) static RESOURCE_METADATA_WRITE_GATE: (
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    );
}

#[cfg(test)]
pub(super) async fn wait_before_resource_metadata_write() {
    if let Ok((entered, release)) = RESOURCE_METADATA_WRITE_GATE
        .try_with(|(entered, release)| (entered.clone(), release.clone()))
    {
        entered.notify_one();
        release.notified().await;
    }
}

pub struct IqConnState<'a> {
    pub carbons_enabled: &'a mut bool,
    pub roster_interested: &'a mut bool,
    pub blocklist_interested: &'a mut bool,
    pub occupancy_session: &'a waddle_xmpp_core::OccupancySessionGeneration,
    pub registry_owner: Option<&'a waddle_xmpp::registry::LocalConnectionBindingId>,
    /// Per-connection [`waddle_xmpp::protocol::XmppStateMachine`].
    /// Required so XEP-0191 block/unblock IQs can mirror their
    /// effect into the dispatcher's session-state snapshot — without
    /// this, additions made on a live connection would not take
    /// effect on the recipient pipeline until the next bind (PR13's
    /// load-at-bind seed). `None` only for transition-period unit
    /// tests; production (`handle_xmpp_frame`) always supplies it.
    pub state_machine: Option<&'a mut waddle_xmpp::protocol::XmppStateMachine>,
    pub ordered_relay_origin: Option<OrderedRelayRouteOrigin>,
}
