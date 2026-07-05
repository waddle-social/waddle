//! Internal test helpers — not exported, used by the in-tree tests in
//! this module and by the top-level `tests/protocol_state_machine.rs`
//! integration test file.

use super::*;
use jid::FullJid;
/// Construct a machine already in the `Ready` phase, for unit tests that
/// want to exercise stanza dispatch without going through the full auth
/// + bind flow.
pub fn ready_machine(
    domain: impl Into<String>,
    full_jid: FullJid,
    dispatcher: StanzaDispatcher,
) -> XmppStateMachine {
    ready_machine_with_id_gen(domain, full_jid, dispatcher, Arc::new(UuidV4Generator))
}

/// Variant of [`ready_machine`] that takes an explicit
/// [`IdGenerator`] — for tests that need deterministic XEP-0359
/// stanza-id stamping (e.g. the L4 wire-trace tests in
/// [`super::super::wire_trace_l4_tests`]).
pub fn ready_machine_with_id_gen(
    domain: impl Into<String>,
    full_jid: FullJid,
    dispatcher: StanzaDispatcher,
    id_gen: Arc<dyn IdGenerator>,
) -> XmppStateMachine {
    XmppStateMachine {
        phase: ConnectionPhase::ready(full_jid, false),
        domain: domain.into(),
        dispatcher,
        next_callback: 0,
        pending_ops: HashMap::new(),
        blocklist: Blocklist::empty(),
        carbons: CarbonsState::Disabled,
        muc_occupancy: MucOccupancy::empty(),
        has_live_transport: true,
        delivery_fanout: Vec::new(),
        id_gen,
        keepalive: KeepalivePolicy::new(KeepaliveConfig::default()),
    }
}
