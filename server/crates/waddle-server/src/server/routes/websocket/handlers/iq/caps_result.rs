//! Inbound IQ result/error matcher for outstanding XEP-0115 caps
//! disco#info queries.
//!
//! When the server has issued a disco#info query because a resource
//! advertised an unknown `(hash, ver)`, the reply travels back over
//! the same websocket. This module peels off that reply *before* the
//! generic IQ-result-ignored path swallows it.
//!
//! The reply is verified per **XEP-0115 §5.4**: the server recomputes
//! the verification string from the response's identities + features
//! and only caches when the recomputed hash matches the advertised
//! `ver`. A mismatch is treated as a poisoning attempt and silently
//! dropped — the caller may re-resolve on a later presence.

use jid::FullJid;
use tracing::{debug, warn};
use waddle_xmpp::disco::info::parse_disco_info_response;

use crate::server::caps_resolution::CapsVerification;
use crate::server::routes::websocket::WebSocketState;

const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";

pub(super) fn handle_caps_disco_info_result(
    iq: &xmpp_parsers::iq::Iq,
    sender: &FullJid,
    state: &WebSocketState,
) {
    let resolver = &state.deps.protocol.caps_resolver;
    let Some(pending) = resolver.take_pending(&iq.id) else {
        return;
    };

    if pending.full_jid != *sender {
        debug!(
            id = %iq.id,
            expected = %pending.full_jid,
            got = %sender,
            "caps disco#info reply arrived from a different resource than the one queried — dropping"
        );
        return;
    }

    let query = match &iq.payload {
        xmpp_parsers::iq::IqType::Result(Some(elem)) => elem,
        xmpp_parsers::iq::IqType::Result(None) => {
            debug!(id = %iq.id, "caps disco#info reply has empty result body");
            return;
        }
        xmpp_parsers::iq::IqType::Error(_) => {
            debug!(id = %iq.id, "caps disco#info reply was an IQ error — not caching");
            return;
        }
        _ => return,
    };

    if query.name() != "query" || query.ns() != NS_DISCO_INFO {
        debug!(id = %iq.id, "caps disco#info reply payload is not a disco#info query element");
        return;
    }

    let parsed = match parse_disco_info_response(query) {
        Ok(parsed) => parsed,
        Err(error) => {
            warn!(error = %error, id = %iq.id, "Failed to parse caps disco#info reply");
            return;
        }
    };

    match resolver.complete_pending(pending, parsed.identities, parsed.features) {
        CapsVerification::Match => {
            debug!(id = %iq.id, jid = %sender, "Cached XEP-0115 caps after hash verification");
        }
        CapsVerification::Mismatch => {
            warn!(
                id = %iq.id,
                jid = %sender,
                "XEP-0115 §5.4: recomputed hash did not match advertised ver — discarding"
            );
        }
    }
}
