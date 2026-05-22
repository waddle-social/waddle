//! IQ handler for XEP-0513 §295 room-permissions query.
//!
//! Targets `<iq type='get' to='room@muc.example'>
//!   <query xmlns='urn:xmpp:mentions:0'/>
//! </iq>` and returns the §303 result form built from the server's
//! hardcoded policy (slices 3a + 3b — `mentions#count = 5`,
//! `mentions#channel = moderators`, `mentions#individual = participants`).
//!
//! ## Why a dedicated handler
//!
//! Slices 3a/3b enforce the policy at T0 candidate classification
//! without exposing the §295 IQ surface — that's spec-conformant under
//! §292 ("Mentions MAY be sent in rooms which do not have permissions
//! set, and/or do not advertise support for them"), but it left the
//! feature advertised state mismatched with the wire shape. CLAUDE.md
//! XEP conformance hard rule: "If Waddle advertises an official XEP
//! feature... the wire shape and behavior MUST conform to that XEP
//! exactly." This handler closes the loop so `Feature::explicit_mentions`
//! and `Feature::channel_mentions` can be re-advertised honestly.
//!
//! ## Authorization
//!
//! GET is open to any **authenticated** requester (including
//! non-occupants of the target room): §303 says the form is what
//! "receiving entities SHOULD refer to when deciding whether to notify
//! the user", so non-occupants legitimately need to read it (e.g. a
//! client deciding whether to render `@channel` autocompletion). We
//! do gate on session-binding — an unauthenticated WebSocket
//! connection that has not completed SASL bind receives
//! `<not-authorized/>` so the §303 policy isn't readable pre-auth
//! (mirrors the auth posture of sibling room-targeted handlers like
//! `pin_query`). We still require the target to be a bare room JID
//! — full-JID and service-JID addressing here are non-sensical and
//! likely client bugs.
//!
//! SET is unconditionally `<forbidden type='auth'/>` per XEP-0513
//! §295's MUST text: *"If the user is not an owner, a `<forbidden/>`
//! error MUST be returned."* Waddle's §303 policy is a server-wide
//! hardcoded default — no user has the "owner of the §295 form"
//! privilege, so every caller is a non-owner and the MUST applies
//! uniformly. Returning `<feature-not-implemented/>` would be more
//! honest about the *cause* (the SET surface is not implemented),
//! but would diverge from the XEP's literal wire-shape requirement;
//! conformance with the spec text wins.
//!
//! ## No room-existence gate
//!
//! Sibling room-targeted IQ handlers (`pin_query`, the MUC disco-info
//! path) look up the room in the registry and return
//! `<item-not-found/>` for unknown room JIDs. The §295 handler
//! deliberately does NOT: the §303 form is a **server-wide**
//! hardcoded policy (`mentions#count = 5`, `mentions#individual =
//! participants`, `mentions#channel = moderators`) that is identical
//! for every room and does not read any per-room state. XEP-0513
//! §292 explicitly contemplates this: "Mentions MAY be sent in rooms
//! which do not have permissions set, and/or do not advertise support
//! for them." Answering a §295 query for a never-instantiated room
//! is therefore both spec-conformant and useful — clients
//! legitimately need the form to know how to render mention UI
//! *before* joining a room.
//!
//! Because every bare-JID query under `muc_domain` yields the same
//! form, the handler is not an existence oracle: it cannot
//! distinguish "exists" from "does not exist" any better than a
//! client sending the same query twice with different JIDs.

use super::*;
use jid::FullJid;
use std::str::FromStr;
use waddle_xmpp::xep::{
    build_mentions_permissions_query, is_mentions_permissions_query, MentionsPermissions,
    NS_EXPLICIT_MENTIONS,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

/// Detects an XEP-0513 §295 query (`<iq type='get'><query
/// xmlns='urn:xmpp:mentions:0'/></iq>` targeting the MUC service
/// domain) or a matching `type='set'` — the dispatcher routes both
/// arms to this handler so the set path produces a §295-shaped error
/// rather than fall through to the generic "Unhandled IQ" path. The
/// handler then enforces the §295 addressing contract (bare room
/// JID) and emits typed `<bad-request/>` for service-JID or
/// full-JID targets, so the client gets a precise diagnostic
/// instead of a vague feature-not-implemented.
pub(super) fn is_mentions_permissions_iq(iq: &Iq, muc_domain: &str) -> bool {
    let payload = match iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
        _ => return false,
    };
    if !is_mentions_permissions_query(payload) {
        return false;
    }
    iq.to().is_some_and(|to| to.domain().as_str() == muc_domain)
}

pub(super) async fn handle_mentions_permissions_iq(
    iq: &Iq,
    _state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    if sender_jid.is_none() {
        // Pre-auth WebSocket connection (no resource bound yet). The
        // §303 form is server-default and stateless, but the
        // auth-posture across sibling room-targeted handlers
        // (e.g. `pin_query`) is "no bound JID → not-authorized"; we
        // mirror that so §295 doesn't become a pre-auth probe
        // surface.
        return vec![mentions_permissions_error(
            iq,
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    }
    let Some(target) = iq.to() else {
        return vec![mentions_permissions_error(
            iq,
            response_from,
            response_to,
            bad_request_iq_error("Mentions-permissions query requires a room JID in 'to'."),
        )];
    };
    if target.node().is_none() {
        // `to='muc.example'` — service-JID target. §295 is per-room;
        // there is no service-wide permissions form.
        return vec![mentions_permissions_error(
            iq,
            response_from,
            response_to,
            bad_request_iq_error(
                "Mentions-permissions query 'to' must be a bare room JID, not the MUC service.",
            ),
        )];
    }
    if target.resource().is_some() {
        // `to='room@muc.example/nick'` — full-JID target. Permissions
        // are room-scoped, not occupant-scoped.
        return vec![mentions_permissions_error(
            iq,
            response_from,
            response_to,
            bad_request_iq_error("Mentions-permissions query 'to' must be a bare room JID."),
        )];
    }

    match iq {
        Iq::Get { .. } => {
            let permissions = MentionsPermissions::server_default();
            let query = build_mentions_permissions_query(&permissions);
            let response = Iq::Result {
                from: response_from.and_then(|s| jid::Jid::from_str(s).ok()),
                to: response_to.and_then(|s| jid::Jid::from_str(s).ok()),
                id: iq.id().to_string(),
                payload: Some(query),
            };
            vec![iq_to_xml(response)]
        }
        Iq::Set { .. } => vec![mentions_permissions_error(
            iq,
            response_from,
            response_to,
            // XEP-0513 §295: "If the user is not an owner, a
            // `<forbidden/>` error MUST be returned." Waddle's
            // server-wide hardcoded policy means no caller can be
            // an owner of the §303 form; every set is from a
            // non-owner and MUST receive `<forbidden type='auth'/>`.
            forbidden_iq_error(
                "Mentions permissions are server-managed; only owners may submit \
                 the §303 form, and no caller is an owner of this form.",
            ),
        )],
        _ => unreachable!("is_mentions_permissions_iq filters to Get/Set"),
    }
}

/// Build a `<iq type='error'/>` for a §295 query that echoes the
/// `<query xmlns='urn:xmpp:mentions:0'/>` payload. The canonical
/// §295 example (xeps/xep-0513.xml §"permissions") shows:
///
/// ```xml
/// <iq from='room@…' to='user@…' type='error' id='…'>
///   <query xmlns='urn:xmpp:mentions:0'/>
///   <error type='auth'>
///     <forbidden xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
///   </error>
/// </iq>
/// ```
///
/// The XEP is Experimental and the example is illustrative — RFC 6120
/// §8.3.1 leaves the original-payload echo as a MAY and does not
/// constrain `<query/>` vs `<error/>` element order. We follow the
/// canonical example as defence-in-depth shape conformance: clients
/// that match the response by name+ns work either way, but emitting
/// the query-before-error order mirrors what an XEP author would
/// reasonably expect to see on the wire.
///
/// Built directly with `Element::builder` (rather than via
/// `Iq::Error` + the xso-derived serializer) because the latter
/// emits `<error/>` before `<payload/>` based on struct field order.
fn mentions_permissions_error(
    iq: &Iq,
    response_from: Option<&str>,
    response_to: Option<&str>,
    error: xmpp_parsers::stanza_error::StanzaError,
) -> String {
    let mut envelope = Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq.id())
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error");
    if let Some(from) = response_from {
        envelope = envelope.attr(minidom::rxml::xml_ncname!("from").to_owned(), from);
    }
    if let Some(to) = response_to {
        envelope = envelope.attr(minidom::rxml::xml_ncname!("to").to_owned(), to);
    }
    let mut envelope = envelope.build();
    envelope.append_child(Element::builder("query", NS_EXPLICIT_MENTIONS).build());
    envelope.append_child(Element::from(error));
    element_to_xml(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iq_get(payload: Element, to: Option<&str>) -> Iq {
        Iq::Get {
            from: None,
            to: to.and_then(|s| jid::Jid::from_str(s).ok()),
            id: "perm-get-1".into(),
            payload,
        }
    }

    fn iq_set(payload: Element, to: Option<&str>) -> Iq {
        Iq::Set {
            from: None,
            to: to.and_then(|s| jid::Jid::from_str(s).ok()),
            id: "perm-set-1".into(),
            payload,
        }
    }

    fn mentions_payload() -> Element {
        Element::builder("query", NS_EXPLICIT_MENTIONS).build()
    }

    #[test]
    fn is_mentions_permissions_iq_accepts_well_formed_get() {
        let iq = iq_get(mentions_payload(), Some("team@muc.example"));
        assert!(is_mentions_permissions_iq(&iq, "muc.example"));
    }

    #[test]
    fn is_mentions_permissions_iq_accepts_set_for_routing() {
        // §295 owner-submitted form. We answer with
        // `<forbidden type='auth'/>` per §295's MUST text — the
        // dispatcher MUST route the IQ here so we own the wire
        // shape (the echoed `<query/>` in the error response
        // matches §295's canonical error example, xep-0513.xml:476).
        let iq = iq_set(mentions_payload(), Some("team@muc.example"));
        assert!(is_mentions_permissions_iq(&iq, "muc.example"));
    }

    #[test]
    fn is_mentions_permissions_iq_rejects_wrong_namespace() {
        let payload = Element::builder("query", "wrong:ns").build();
        let iq = iq_get(payload, Some("team@muc.example"));
        assert!(!is_mentions_permissions_iq(&iq, "muc.example"));
    }

    #[test]
    fn is_mentions_permissions_iq_rejects_wrong_element_name() {
        let payload = Element::builder("permissions", NS_EXPLICIT_MENTIONS).build();
        let iq = iq_get(payload, Some("team@muc.example"));
        assert!(!is_mentions_permissions_iq(&iq, "muc.example"));
    }

    #[test]
    fn is_mentions_permissions_iq_rejects_missing_to() {
        let iq = iq_get(mentions_payload(), None);
        assert!(!is_mentions_permissions_iq(&iq, "muc.example"));
    }

    #[test]
    fn is_mentions_permissions_iq_rejects_wrong_domain() {
        let iq = iq_get(mentions_payload(), Some("team@other.example"));
        assert!(!is_mentions_permissions_iq(&iq, "muc.example"));
    }

    #[test]
    fn is_mentions_permissions_iq_rejects_iq_result_and_error() {
        let payload = mentions_payload();
        let result = Iq::Result {
            from: None,
            to: jid::Jid::from_str("team@muc.example").ok(),
            id: "perm-1".into(),
            payload: Some(payload.clone()),
        };
        assert!(!is_mentions_permissions_iq(&result, "muc.example"));

        let error = Iq::Error {
            from: None,
            to: jid::Jid::from_str("team@muc.example").ok(),
            id: "perm-1".into(),
            error: xmpp_parsers::stanza_error::StanzaError::new(
                xmpp_parsers::stanza_error::ErrorType::Cancel,
                xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable,
                "en",
                "unrelated",
            ),
            payload: Some(payload),
        };
        assert!(!is_mentions_permissions_iq(&error, "muc.example"));
    }

    /// The routing predicate is intentionally permissive on `to`
    /// (domain + namespace), so the service-JID case
    /// (`to='muc.example'`) reaches the handler and gets a typed
    /// `<bad-request/>` rather than falling through. Pinning the
    /// predicate keeps that contract explicit.
    #[test]
    fn is_mentions_permissions_iq_routes_service_jid_for_typed_error() {
        let iq = iq_get(mentions_payload(), Some("muc.example"));
        assert!(is_mentions_permissions_iq(&iq, "muc.example"));
    }

    /// Same rationale for the full-JID case: the predicate routes,
    /// the handler validates and returns `<bad-request/>`.
    #[test]
    fn is_mentions_permissions_iq_routes_full_jid_for_typed_error() {
        let iq = iq_get(mentions_payload(), Some("team@muc.example/nick"));
        assert!(is_mentions_permissions_iq(&iq, "muc.example"));
    }

    fn build_error(stanza_error: xmpp_parsers::stanza_error::StanzaError) -> String {
        let iq = iq_get(mentions_payload(), Some("team@muc.example"));
        mentions_permissions_error(
            &iq,
            Some("team@muc.example"),
            Some("user@example.com/resource"),
            stanza_error,
        )
    }

    /// XEP-0513 §295 error example (xep-0513.xml §"permissions"):
    /// the response carries an empty `<query
    /// xmlns='urn:xmpp:mentions:0'/>` echoed alongside `<error/>`.
    /// RFC 6120 §8.3.1 leaves payload-echo as a MAY and does not
    /// constrain element order; XEP §295 is Experimental and the
    /// example is illustrative. The handler still emits the echo in
    /// query-before-error order as defence-in-depth shape conformance
    /// — a client that pattern-matches positionally against the XEP
    /// example will accept the response either way.
    #[test]
    fn mentions_permissions_error_envelope_echoes_empty_query() {
        let xml = build_error(forbidden_iq_error("non-owner"));
        let parsed = Element::from_str(&xml).expect("error iq parses");
        assert_eq!(parsed.name(), "iq");
        assert_eq!(parsed.attr("type"), Some("error"));
        // §295 echo: the `<query xmlns='urn:xmpp:mentions:0'/>` child
        // appears as a sibling of `<error/>`.
        let query = parsed
            .get_child("query", NS_EXPLICIT_MENTIONS)
            .expect("§295 error envelope echoes <query/>");
        assert_eq!(
            query.children().count(),
            0,
            "echoed query MUST be empty per the §295 error example"
        );
        // `<error/>` carries the typed condition.
        let error = parsed
            .children()
            .find(|c| c.name() == "error")
            .expect("error element present");
        assert!(error.has_child("forbidden", "urn:ietf:params:xml:ns:xmpp-stanzas",));
        // §295 MUSTs `type='auth'` for the `<forbidden/>` condition
        // (xeps/xep-0513.xml line 482).
        assert_eq!(error.attr("type"), Some("auth"));

        // Positional pin: §295 example shows `<query/>` BEFORE
        // `<error/>`. The xmpp_parsers `Iq::Error` serializer emits
        // the struct fields in declaration order (error first), so
        // building the envelope directly via `Element::builder` +
        // `append_child` (as `mentions_permissions_error` does) is
        // load-bearing for matching the canonical example.
        let names: Vec<&str> = parsed.children().map(Element::name).collect();
        assert_eq!(
            names,
            vec!["query", "error"],
            "§295 example shows <query/> before <error/>; \
             child order must match the canonical wire shape"
        );
    }

    /// Pre-auth path: when the WebSocket connection has not yet
    /// completed SASL bind, `sender_jid` is `None`. The handler MUST
    /// respond with `<not-authorized type='auth'/>` rather than
    /// expose the §303 policy form to an unbound session — mirrors
    /// the auth posture of sibling room-targeted handlers
    /// (`pin_query` does the same). This test pins the shape of the
    /// error envelope produced on that path.
    #[test]
    fn mentions_permissions_unauthenticated_error_envelope_is_not_authorized() {
        let xml = build_error(not_authorized_iq_error("Authentication required."));
        let parsed = Element::from_str(&xml).expect("error iq parses");
        let error = parsed
            .children()
            .find(|c| c.name() == "error")
            .expect("error element present");
        assert!(error.has_child("not-authorized", "urn:ietf:params:xml:ns:xmpp-stanzas"));
        // RFC 6120 §8.3.3.13: not-authorized travels with type='auth'.
        assert_eq!(error.attr("type"), Some("auth"));
    }

    // The full happy GET → §303 form path is exercised end-to-end by
    // the WebSocket integration test
    // `mentions_permissions_iq_get_returns_303_form` in
    // `tests/xep0513_mentions_ws.rs`; that test stands up a real
    // `WebSocketState`, which a unit test cannot do without
    // duplicating the entire bootstrap. The unit tests above pin the
    // routing predicate and the error-envelope shape, which is the
    // logic actually under the handler's local control.
}
