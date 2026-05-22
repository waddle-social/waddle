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
//! GET is open to any participant: §303 says the form is what
//! "receiving entities SHOULD refer to when deciding whether to notify
//! the user", so non-occupants legitimately need to read it (e.g. a
//! client deciding whether to render `@channel` autocompletion). We
//! still require the target to be a bare room JID — full-JID
//! addressing here is non-sensical and likely a client bug.
//!
//! SET is unconditionally `<feature-not-implemented/>`. XEP §295
//! reserves `<forbidden/>` for "the user is not an owner", but Waddle
//! exposes no owner-driven config mutation for the §303 form; the
//! policy is a server-internal hardcoded default. Returning forbidden
//! would lie about *why* the set failed.

use super::*;
use jid::FullJid;
use std::str::FromStr;
use waddle_xmpp::xep::{
    build_mentions_permissions_query, is_mentions_permissions_query, MentionsPermissions,
    NS_EXPLICIT_MENTIONS,
};
use xmpp_parsers::iq::Iq;

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
    _sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
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
            feature_not_implemented_iq_error(
                "Mentions permissions are server-hardcoded; the §303 form is read-only.",
            ),
        )],
        _ => unreachable!("is_mentions_permissions_iq filters to Get/Set"),
    }
}

/// Build a `<iq type='error'/>` for a §295 query that ECHOES the
/// original `<query xmlns='urn:xmpp:mentions:0'/>` payload, per the
/// XEP-0513 §295 error example (xeps/xep-0513.xml §"permissions"):
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
/// RFC 6120 §8.3.1 makes the original-payload echo a MAY for stanza
/// errors in general; XEP-0513 §295 elevates it for this IQ by
/// showing it in the canonical example. Echoing the empty
/// `<query xmlns='urn:xmpp:mentions:0'/>` (not the full request
/// payload — RFC says "the original XML which caused the error" but
/// the §295 example uses the empty-query shape) makes the response
/// recognisable as the §295 error contract.
fn mentions_permissions_error(
    iq: &Iq,
    response_from: Option<&str>,
    response_to: Option<&str>,
    error: xmpp_parsers::stanza_error::StanzaError,
) -> String {
    let response = Iq::Error {
        from: response_from.and_then(|s| jid::Jid::from_str(s).ok()),
        to: response_to.and_then(|s| jid::Jid::from_str(s).ok()),
        id: iq.id().to_string(),
        error,
        payload: Some(
            xmpp_parsers::minidom::Element::builder("query", NS_EXPLICIT_MENTIONS).build(),
        ),
    };
    iq_to_xml(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::minidom::Element;

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
        // §295 owner-submitted form. Even though we'll answer with
        // feature-not-implemented, the dispatcher MUST route the IQ
        // here so we own the wire shape (echoed `<query/>` in the
        // error response is consistent with §295's error example).
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
    /// RFC 6120 §8.3.1 makes payload-echo on errors a MAY; the XEP
    /// elevates it for this IQ by showing it in the canonical
    /// example, so the handler MUST emit it.
    #[test]
    fn mentions_permissions_error_envelope_echoes_empty_query() {
        let xml = build_error(feature_not_implemented_iq_error("read-only"));
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
        assert!(error.has_child(
            "feature-not-implemented",
            "urn:ietf:params:xml:ns:xmpp-stanzas",
        ));
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
