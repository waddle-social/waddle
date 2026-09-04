pub struct IqConnState<'a> {
    pub carbons_enabled: &'a mut bool,
    pub roster_interested: &'a mut bool,
    pub blocklist_interested: &'a mut bool,
    pub occupancy_session: &'a waddle_xmpp_core::OccupancySessionGeneration,
    pub registry_owner: Option<&'a std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Per-connection [`waddle_xmpp::protocol::XmppStateMachine`].
    /// Required so XEP-0191 block/unblock IQs can mirror their
    /// effect into the dispatcher's session-state snapshot — without
    /// this, additions made on a live connection would not take
    /// effect on the recipient pipeline until the next bind (PR13's
    /// load-at-bind seed). `None` only for transition-period unit
    /// tests; production (`handle_xmpp_frame`) always supplies it.
    pub state_machine: Option<&'a mut waddle_xmpp::protocol::XmppStateMachine>,
    pub ordered_relay_origin: Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
}
