use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::Stanza;

use super::PromotedOutcome;

/// Parse a wire-XML stanza back to its typed [`Stanza`] variant.
/// Returns `None` for unparseable XML or unknown root elements.
/// Handles `<message/>`, `<iq/>`, and `<presence/>` — the three
/// stanza kinds the SM unacked queue can hold (Copilot review on
/// PR #346: previous code only parsed `<message/>` and silently
/// dropped IQ/presence as Unparseable).
pub(super) fn parse_stanza(xml: &str) -> Option<Stanza> {
    let element: xmpp_parsers::minidom::Element = xml.parse().ok()?;
    match element.name() {
        "message" => xmpp_parsers::message::Message::try_from(element)
            .ok()
            .map(Stanza::Message),
        "iq" => xmpp_parsers::iq::Iq::try_from(element).ok().map(Stanza::Iq),
        "presence" => xmpp_parsers::presence::Presence::try_from(element)
            .ok()
            .map(Stanza::Presence),
        _ => None,
    }
}

/// Promote an unacked `<iq/>` per the unavailable-resource semantics.
/// IQs cannot be queued offline (XEP-0160 §3 narrative explicitly
/// scopes offline storage to message stanzas; XEP-0160 §1 line 63
/// excludes IQ/presence). Try alt-resource live-redelivery; drop
/// otherwise.
pub(super) async fn promote_iq(
    iq: xmpp_parsers::iq::Iq,
    registry: &ConnectionRegistry,
) -> PromotedOutcome {
    let target = iq
        .to
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
        .filter(|full| registry.get_entry(full).is_some());
    if let Some(target) = target {
        if matches!(
            registry.send_to(&target, Stanza::Iq(iq)).await,
            SendResult::Sent
        ) {
            return PromotedOutcome::Redelivered { to: target };
        }
    }
    PromotedOutcome::Dropped
}

/// Promote an unacked `<presence/>` per the unavailable-resource
/// semantics. Presence is not stored offline (RFC 6121 §8.5.2.1.4).
/// Try alt-resource live-redelivery; drop otherwise.
pub(super) async fn promote_presence(
    presence: xmpp_parsers::presence::Presence,
    registry: &ConnectionRegistry,
) -> PromotedOutcome {
    let target = presence
        .to
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
        .filter(|full| registry.get_entry(full).is_some());
    if let Some(target) = target {
        if matches!(
            registry.send_to(&target, Stanza::Presence(presence)).await,
            SendResult::Sent
        ) {
            return PromotedOutcome::Redelivered { to: target };
        }
    }
    PromotedOutcome::Dropped
}
