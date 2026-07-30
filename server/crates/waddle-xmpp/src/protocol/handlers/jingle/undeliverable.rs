//! Compensation and echo sanitization for 1:1 Jingle negotiation IQs
//! that turn out undeliverable after forwarding (#1444).
//!
//! [`super::JingleHandler`] mints the addressee's join token and
//! injects it into the forwarded `session-initiate` / `session-accept`
//! BEFORE routability is known. When delivery then fails, the routing
//! layer uses these helpers to derive what to roll back and what is
//! safe to echo in the RFC 6120 §8.3.1 error.

use minidom::Element;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jingle::{Action, Jingle};

use waddle_sfu::{CallId, Identity, Jti};

use super::scoped_call_id;
use crate::xep::xep0166::NS_JINGLE;
use crate::xep::xep0272::find_muji;
use crate::xep::xep_waddle_livekit_transport::{
    WaddleLiveKitTransport, NS_WADDLE_LIVEKIT_TRANSPORT, TRANSPORT_NAME,
};

/// The `(call, identity)` whose freshly minted LiveKit join credentials
/// must be revoked when a forwarded 1:1 Jingle negotiation IQ turns out
/// undeliverable (#1444).
pub struct UndeliverableNegotiationRollback {
    pub call_id: CallId,
    pub identity: Identity,
    /// The jti of the join token the bounced stanza carried, when its
    /// `urn:waddle:transports:livekit:0` transport holds an issued
    /// credential. Compensation revokes exactly this issuance
    /// ([`waddle_sfu::SfuService::revoke_issued_token`]) — never the
    /// pair's other tokens or its registration, which may belong to a
    /// live session from an independent, successful negotiation.
    pub minted_jti: Option<Jti>,
}

/// Derive the rollback pair from the bounced stanza itself. Returns
/// `None` for anything that is not a forwarded 1:1
/// `session-initiate`/`session-accept` addressed to a full JID.
///
/// The call scoping mirrors `JingleHandler::handle_session_negotiation`:
/// an initiate is scoped to its sender (the initiator), an accept to its
/// addressee (the initiator being handed the responder's accept) — and
/// the token was minted for the addressee in both cases. Muji-bearing
/// stanzas (XEP-0272 `<muji/>` child) yield `None`: their calls are
/// room-scoped, so the `{bare}::{sid}` derivation would be wrong — and
/// since `unregister_call_participant` also fires a LiveKit
/// `RemoveParticipant`, a client-chosen sid colliding with the sender's
/// own live 1:1 call must never trigger a misdirected eviction.
pub fn undeliverable_negotiation_rollback(iq: &Iq) -> Option<UndeliverableNegotiationRollback> {
    let Iq::Set {
        from: Some(from),
        to: Some(to),
        payload,
        ..
    } = iq
    else {
        return None;
    };
    if !payload.is("jingle", NS_JINGLE) {
        return None;
    }
    if find_muji(payload).is_some() {
        return None;
    }
    let jingle = Jingle::try_from(payload.clone()).ok()?;
    let to_full = to.clone().try_into_full().ok()?;
    let initiator_bare = match jingle.action {
        Action::SessionInitiate => from.to_bare(),
        Action::SessionAccept => to_full.to_bare(),
        _ => return None,
    };
    let call_id = scoped_call_id(&initiator_bare, &jingle.sid.0).ok()?;
    Some(UndeliverableNegotiationRollback {
        call_id,
        identity: Identity::from_jid(to_full),
        minted_jti: minted_jti_in_payload(payload),
    })
}

/// The jti of the (single, #1142) server-issued LiveKit token inside a
/// forwarded negotiation payload, decoded without signature
/// verification — it only keys the server's own issuance bookkeeping.
fn minted_jti_in_payload(payload: &Element) -> Option<Jti> {
    payload
        .children()
        .flat_map(|content| content.children())
        .find_map(|elem| match WaddleLiveKitTransport::try_from(elem) {
            Ok(WaddleLiveKitTransport::Issued(issued)) => issued.token.unverified_jti(),
            _ => None,
        })
}

/// A copy of a bounced Jingle payload that is safe to echo per RFC
/// 6120 §8.3.1 (#1444): everything the SENDER supplied stays verbatim
/// — the echo returns their own request — and the only material
/// removed is the server-injected `urn:waddle:transports:livekit:0`
/// transport, the sole element that can carry credentials the sender
/// was never meant to hold.
pub fn credential_free_jingle_echo(payload: &Element) -> Element {
    let mut echo = payload.clone();
    for content in echo.children_mut() {
        while content
            .remove_child(TRANSPORT_NAME, NS_WADDLE_LIVEKIT_TRANSPORT)
            .is_some()
        {}
    }
    // Defensive: strip a (malformed) top-level transport too.
    while echo
        .remove_child(TRANSPORT_NAME, NS_WADDLE_LIVEKIT_TRANSPORT)
        .is_some()
    {}
    echo
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0166;

    fn negotiation_iq(action: &str, from: &str, to: &str, sid: &str) -> Iq {
        let payload = Element::builder("jingle", xep0166::NS_JINGLE)
            .attr(minidom::rxml::xml_ncname!("action").to_owned(), action)
            .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid)
            .build();
        Iq::Set {
            from: Some(from.parse().expect("from jid")),
            to: Some(to.parse().expect("to jid")),
            id: "bounce-1".to_string(),
            payload,
        }
    }

    #[test]
    fn undeliverable_initiate_rollback_targets_the_addressee_scoped_to_the_sender() {
        let iq = negotiation_iq(
            "session-initiate",
            "alice@waddle.test/web",
            "bob@waddle.test/phone",
            "c1",
        );
        let rollback = undeliverable_negotiation_rollback(&iq).expect("initiate yields rollback");
        assert_eq!(rollback.call_id.as_str(), "alice@waddle.test::c1");
        assert_eq!(
            rollback.identity.as_jid().to_string(),
            "bob@waddle.test/phone"
        );
    }

    #[test]
    fn undeliverable_accept_rollback_scopes_the_call_to_the_addressed_initiator() {
        // A bounced accept was travelling responder → initiator; the
        // token inside was minted for the initiator, and the call is
        // scoped to that initiator's bare JID.
        let iq = negotiation_iq(
            "session-accept",
            "bob@waddle.test/phone",
            "alice@waddle.test/web",
            "c1",
        );
        let rollback = undeliverable_negotiation_rollback(&iq).expect("accept yields rollback");
        assert_eq!(rollback.call_id.as_str(), "alice@waddle.test::c1");
        assert_eq!(
            rollback.identity.as_jid().to_string(),
            "alice@waddle.test/web"
        );
    }

    #[test]
    fn non_negotiation_jingle_actions_yield_no_rollback() {
        for action in ["session-info", "session-terminate", "transport-info"] {
            let iq = negotiation_iq(
                action,
                "alice@waddle.test/web",
                "bob@waddle.test/phone",
                "c1",
            );
            assert!(
                undeliverable_negotiation_rollback(&iq).is_none(),
                "{action} must not trigger a token rollback"
            );
        }
    }

    #[test]
    fn muji_bearing_accept_yields_no_rollback() {
        // A Muji (XEP-0272) session-accept is room-scoped; deriving the
        // 1:1 `{bare}::{sid}` shape from it would revoke — and evict,
        // via the SFU RemoveParticipant leg — a same-sid 1:1 call.
        let mut iq = negotiation_iq(
            "session-accept",
            "calls.waddle.test",
            "alice@waddle.test/web",
            "c1",
        );
        if let Iq::Set { payload, .. } = &mut iq {
            let muji = Element::builder("muji", crate::xep::xep0272::NS_MUJI)
                .attr(
                    minidom::rxml::xml_ncname!("room").to_owned(),
                    "room@muc.waddle.test",
                )
                .build();
            *payload = Element::builder("jingle", xep0166::NS_JINGLE)
                .attr(
                    minidom::rxml::xml_ncname!("action").to_owned(),
                    "session-accept",
                )
                .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "c1")
                .append(muji)
                .build();
        }
        assert!(undeliverable_negotiation_rollback(&iq).is_none());
    }

    #[test]
    fn bare_jid_addressee_yields_no_rollback() {
        // Tokens are only minted for full-JID addressees; a bare `to`
        // (e.g. a Muji mixer route) has nothing to revoke.
        let iq = negotiation_iq(
            "session-initiate",
            "alice@waddle.test/web",
            "bob@waddle.test",
            "c1",
        );
        assert!(undeliverable_negotiation_rollback(&iq).is_none());
    }
}
