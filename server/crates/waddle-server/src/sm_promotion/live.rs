use jid::{BareJid, FullJid};
use waddle_xmpp::protocol::dm_routing::{DmRouting, LiveDecision, OnlineResources};
use waddle_xmpp::registry::ConnectionRegistry;

/// Build an [`OnlineResources`] snapshot for `recipient_bare` from
/// the connection registry.
///
/// Filters to resources that are both connected AND have sent
/// available presence. RFC 6121 §8.5.2.1.1 says only "available
/// resources that have specified a non-negative priority" are
/// candidates for bare-JID delivery; classify_dm_intake's online-
/// check should match. Without the `presence_available` filter
/// (Qodo review on PR #346), a connected-but-unavailable resource
/// would be mis-classified as a live recipient and the unacked
/// stanza would silently fail to route.
pub(super) fn build_online_resources(
    registry: &ConnectionRegistry,
    recipient_bare: &BareJid,
) -> OnlineResources {
    let pairs: Vec<(FullJid, i8)> = registry
        .get_resources_for_user(recipient_bare)
        .into_iter()
        .filter_map(|full| {
            let entry = registry.get_entry(&full)?;
            if !entry.is_presence_available() {
                return None;
            }
            Some((full, entry.presence_priority()))
        })
        .collect();
    OnlineResources::from_pairs(pairs)
}

/// Collect every live-delivery target per the classifier's
/// `LiveDecision`. Returns an empty vec if no online resource
/// matches.
///
/// For `DeliverToFull`: if the addressed full JID is connected,
/// route there only. If it isn't (the original-detached resource is
/// gone), fall back to RFC 6121 §8.5.3's bare-JID fanout — locked
/// Q6 = B intent: the message gets to SOME resource of the
/// recipient, not just the original target.
///
/// For `DeliverToBareWithFanout`: route to ALL non-negative-priority
/// resources, matching `interpret.rs`'s live-route fanout (Copilot
/// review on PR #346: earlier code took only the first via `next()`
/// which lost deliveries on multi-resource users).
pub(super) fn collect_live_targets(
    routing: &DmRouting,
    message: &xmpp_parsers::message::Message,
    registry: &ConnectionRegistry,
) -> Vec<FullJid> {
    let bare_target = match message.to.as_ref() {
        Some(jid) => jid.to_bare(),
        None => return Vec::new(),
    };
    match routing.live {
        LiveDecision::None => Vec::new(),
        LiveDecision::DeliverToFull => {
            let full_target = message
                .to
                .as_ref()
                .and_then(|jid| jid.clone().try_into_full().ok())
                .filter(|full| registry.get_entry(full).is_some());
            if let Some(full) = full_target {
                vec![full]
            } else {
                // Addressed resource has gone offline since the
                // classifier ran (or before promotion fired).
                // Fall back to bare-JID fanout per RFC 6121 §8.5.3
                // ("treat as if addressed to bare JID").
                registry.select_routable_resources_for_user(&bare_target)
            }
        }
        LiveDecision::DeliverToBareWithFanout => {
            registry.select_routable_resources_for_user(&bare_target)
        }
    }
}
