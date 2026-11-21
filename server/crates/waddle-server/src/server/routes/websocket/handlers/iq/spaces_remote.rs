//! Changes to a Space asked by a principal at another server (ADR-0020
//! D8 and D10; XEP-0060).
//!
//! The Spaces host decides peer admission, that the Space is open to the
//! principal, and that the principal's privilege is honoured for its stream
//! before it calls [`spaces_change_for_remote`]; what remains is the
//! decision a session's request meets, taken for the remote account as its
//! subject.

use jid::BareJid;
use waddle_xmpp::pubsub::{build_pubsub_error, PubSubError, PubSubRequest};
use xmpp_parsers::iq::Iq;

use super::pubsub_admin::handle_pubsub_admin_request;
use super::{
    handle_spaces_publish, handle_spaces_retract, spaces_service_bare_jid, SpacesRetract,
    WebSocketState,
};
use crate::auth::Session;
use crate::server::routes::websocket::ResolvedPrincipal;

/// Decide `request`, a change to a Space that the remote account `account`
/// asked for in `iq`, as a session's request is decided, and answer it with
/// the typed IQs the session handlers build.
///
/// - A publish or a retract places a room in the Space or takes it out. It
///   is decided by the Space's `owner` relationship or the deployment's
///   right to create Spaces, and by authority over the room, for `account`
///   as the subject of those checks. A publish or a retract of the Space's
///   avatar or banner item (XEP-0503 0.3.0, "Spaces images") names no room
///   and is decided by the first of those alone.
/// - A request of the `pubsub#owner` namespace is decided by the node's
///   Owner affiliation. It is made without a session, so it neither reads
///   nor changes whether the Space is open to other servers.
///
/// Creating a node is not among them: the host refuses it before this.
pub(crate) async fn spaces_change_for_remote(
    state: &WebSocketState,
    iq: &Iq,
    account: &BareJid,
    request: PubSubRequest,
) -> Vec<Iq> {
    let domains = &state.deps.service_domains;
    let Ok(spaces_jid) = spaces_service_bare_jid(&domains.spaces) else {
        return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
    };
    // The placement checks read their subject from a session. This one
    // names the remote account and nothing else: it is no credential, is
    // stored nowhere, and the host has already decided the request on the
    // evidence of its stream.
    let localpart = account
        .node()
        .map_or_else(|| account.to_string(), |node| node.to_string());
    let named = Session::new(account.as_str(), &localpart, &localpart);
    match request {
        PubSubRequest::Publish { node, item, .. } => {
            handle_spaces_publish(
                iq,
                state,
                &domains.muc,
                &domains.spaces,
                &node,
                item,
                Some(ResolvedPrincipal::from_authenticated_session(&named)),
            )
            .await
        }
        PubSubRequest::Retract {
            node,
            item_id,
            notify,
        } => {
            handle_spaces_retract(
                iq,
                state,
                &domains.muc,
                &domains.spaces,
                SpacesRetract {
                    node: &node,
                    item_id: &item_id,
                    notify,
                },
                Some(ResolvedPrincipal::from_authenticated_session(&named)),
            )
            .await
        }
        PubSubRequest::CreateNode { .. } => {
            vec![build_pubsub_error(iq, PubSubError::Forbidden)]
        }
        request => {
            handle_pubsub_admin_request(
                iq,
                state,
                &spaces_jid,
                account,
                &domains.spaces,
                None,
                request,
            )
            .await
        }
    }
}
