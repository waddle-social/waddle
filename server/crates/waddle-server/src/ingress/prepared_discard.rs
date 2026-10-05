//! Keep a prepared no-store copy owed when its exact target is unavailable.
//!
//! A reachability snapshot cannot fence a reconnect through receipt commit.
//! Only actual delivery/custody or an independent policy proof may complete it.
use super::recorded::{ProgressObligation, RouteProgress};
use jid::FullJid;
use waddle_xmpp::xep::xep0334::{has_hint, Hint};

pub(super) fn forbids_offline_handoff(progress: &RouteProgress) -> bool {
    protected_target(progress).is_some()
}

fn protected_target(progress: &RouteProgress) -> Option<&FullJid> {
    let ProgressObligation::Direct {
        prepared: Some(prepared),
        ..
    } = &progress.obligation
    else {
        return None;
    };
    let message = prepared.message();
    let target = message.to.as_ref()?.try_as_full().ok()?;
    (progress.fanout.as_slice() == std::slice::from_ref(target)
        && message.from.as_ref()?.to_bare() != target.to_bare()
        && has_hint(message, Hint::NoStore)
        && !has_hint(message, Hint::Store))
    .then_some(target)
}
