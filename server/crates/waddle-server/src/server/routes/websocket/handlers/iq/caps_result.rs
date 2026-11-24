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
use waddle_xmpp::disco::info::{parse_disco_info_response, DISCO_INFO_NS};

use crate::server::caps_resolution::CapsVerification;
use crate::server::routes::websocket::WebSocketState;

pub(super) async fn handle_caps_disco_info_result(
    iq: &xmpp_parsers::iq::Iq,
    sender: &FullJid,
    state: &WebSocketState,
) {
    let resolver = &state.deps.protocol.caps_resolver;
    let Some(pending) = resolver.take_pending(iq.id()) else {
        return;
    };

    if pending.full_jid != *sender {
        debug!(
            id = %iq.id(),
            expected = %pending.full_jid,
            got = %sender,
            "caps disco#info reply arrived from a different resource than the one queried — dropping"
        );
        return;
    }

    let query = match iq {
        xmpp_parsers::iq::Iq::Result {
            payload: Some(elem),
            ..
        } => elem,
        xmpp_parsers::iq::Iq::Result { payload: None, .. } => {
            debug!(id = %iq.id(), "caps disco#info reply has empty result body");
            return;
        }
        xmpp_parsers::iq::Iq::Error { .. } => {
            debug!(id = %iq.id(), "caps disco#info reply was an IQ error — not caching");
            return;
        }
        _ => return,
    };

    if query.name() != "query" || query.ns() != DISCO_INFO_NS {
        debug!(id = %iq.id(), "caps disco#info reply payload is not a disco#info query element");
        return;
    }

    let parsed = match parse_disco_info_response(query) {
        Ok(parsed) => parsed,
        Err(error) => {
            warn!(error = %error, id = %iq.id(), "Failed to parse caps disco#info reply");
            return;
        }
    };

    let previous_features: std::collections::HashSet<String> = resolver
        .key_for_resource(sender)
        .and_then(|key| resolver.cached(&key))
        .map(|cached| cached.features.into_iter().map(|f| f.0).collect())
        .unwrap_or_default();

    match resolver.complete_pending(
        pending,
        parsed.identities,
        parsed.features,
        parsed.extensions,
        parsed.ill_formed,
    ) {
        CapsVerification::Match => {
            debug!(id = %iq.id(), jid = %sender, "Cached XEP-0115 caps after hash verification");
            // XEP-0163: interest became known after the initial presence;
            // replay last items for nodes not already covered by the
            // previously known caps, to a resource that is still available.
            let still_available =
                crate::server::routes::websocket::handlers::account_reads::available_resources(
                    state,
                    &sender.to_bare(),
                )
                .await
                .iter()
                .any(|(jid, _)| jid == sender);
            if still_available {
                crate::server::routes::websocket::handlers::pubsub_fanout::replay_last_published_items(
                    state,
                    sender,
                    &previous_features,
                )
                .await;
            }
        }
        CapsVerification::Mismatch => {
            warn!(
                id = %iq.id(),
                jid = %sender,
                "XEP-0115 §5.4: recomputed hash did not match advertised ver (or unsupported algo) — discarding"
            );
        }
        CapsVerification::IllFormed => {
            warn!(
                id = %iq.id(),
                jid = %sender,
                "XEP-0115 §5.4 step 2.4: ill-formed disco#info response — discarding entire reply"
            );
        }
    }
}
