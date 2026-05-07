//! Typed `<iq type='error'>` constructors for the IQ-handler tree.
//!
//! Per the typed-payloads hard rule (CLAUDE.md), IQ-error responses
//! MUST be modelled with `xmpp_parsers::stanza_error::StanzaError`
//! (typed `ErrorType` + `DefinedCondition` enum variants), not as
//! stringly-typed `&str` flags shuffled across function boundaries.
//!
//! This module groups the per-condition `StanzaError` constructors used
//! across the WebSocket IQ handlers. Each helper returns a `StanzaError`
//! ready to be passed to [`super::super::super::build_iq_error_xml_typed`]
//! together with the `<iq>` `id` and addressing metadata.

use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Default human-readable language tag attached to every typed
/// IQ-error helper. The XEP only requires text bodies be tagged when
/// present; we use English for diagnostics.
const DEFAULT_LANG: &str = "en";

/// `<bad-request/>` (modify) — malformed payload, e.g. unparseable
/// child element.
pub(crate) fn bad_request_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Modify,
        DefinedCondition::BadRequest,
        DEFAULT_LANG,
        text,
    )
}

/// `<jid-malformed/>` (modify) — `<iq to=…>` carried a JID the parser
/// rejected.
pub(crate) fn jid_malformed_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Modify,
        DefinedCondition::JidMalformed,
        DEFAULT_LANG,
        text,
    )
}

/// `<not-acceptable/>` (modify) — request well-formed but the
/// recipient refuses based on policy.
pub(crate) fn not_acceptable_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Modify,
        DefinedCondition::NotAcceptable,
        DEFAULT_LANG,
        text,
    )
}

/// `<not-authorized/>` (auth) — the request requires authentication
/// the sender has not provided.
pub(crate) fn not_authorized_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Auth,
        DefinedCondition::NotAuthorized,
        DEFAULT_LANG,
        text,
    )
}

/// `<forbidden/>` (auth) — the sender is authenticated but lacks
/// permission for the requested operation.
pub(crate) fn forbidden_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Auth,
        DefinedCondition::Forbidden,
        DEFAULT_LANG,
        text,
    )
}

/// `<item-not-found/>` (cancel) — the addressed JID, node, or item
/// does not exist.
pub(crate) fn item_not_found_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        DEFAULT_LANG,
        text,
    )
}

/// `<service-unavailable/>` (cancel) — the addressed entity does not
/// implement the requested feature/namespace at this address.
pub(crate) fn service_unavailable_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ServiceUnavailable,
        DEFAULT_LANG,
        text,
    )
}

/// `<feature-not-implemented/>` (cancel) — the namespace is recognised
/// but this specific feature/profile is not implemented.
pub(crate) fn feature_not_implemented_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::FeatureNotImplemented,
        DEFAULT_LANG,
        text,
    )
}

/// `<internal-server-error/>` (wait) — unexpected failure in the
/// server while processing an otherwise-valid request.
pub(crate) fn internal_server_error_iq_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        DEFAULT_LANG,
        text,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::minidom::Element;

    fn assert_round_trip(error: StanzaError, expected_type: ErrorType, expected: DefinedCondition) {
        assert_eq!(error.type_, expected_type);
        assert_eq!(error.defined_condition, expected);
        let element = Element::from(error);
        assert_eq!(element.name(), "error");
        assert_eq!(element.ns(), "jabber:client");
        let condition = element
            .children()
            .find(|child| {
                child.ns() == "urn:ietf:params:xml:ns:xmpp-stanzas" && child.name() != "text"
            })
            .expect("defined-condition child");
        assert_eq!(condition.ns(), "urn:ietf:params:xml:ns:xmpp-stanzas");
    }

    #[test]
    fn bad_request_typed() {
        assert_round_trip(
            bad_request_iq_error("nope"),
            ErrorType::Modify,
            DefinedCondition::BadRequest,
        );
    }

    #[test]
    fn jid_malformed_typed() {
        assert_round_trip(
            jid_malformed_iq_error("nope"),
            ErrorType::Modify,
            DefinedCondition::JidMalformed,
        );
    }

    #[test]
    fn not_acceptable_typed() {
        assert_round_trip(
            not_acceptable_iq_error("nope"),
            ErrorType::Modify,
            DefinedCondition::NotAcceptable,
        );
    }

    #[test]
    fn not_authorized_typed() {
        assert_round_trip(
            not_authorized_iq_error("nope"),
            ErrorType::Auth,
            DefinedCondition::NotAuthorized,
        );
    }

    #[test]
    fn forbidden_typed() {
        assert_round_trip(
            forbidden_iq_error("nope"),
            ErrorType::Auth,
            DefinedCondition::Forbidden,
        );
    }

    #[test]
    fn item_not_found_typed() {
        assert_round_trip(
            item_not_found_iq_error("nope"),
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
        );
    }

    #[test]
    fn service_unavailable_typed() {
        assert_round_trip(
            service_unavailable_iq_error("nope"),
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
        );
    }

    #[test]
    fn feature_not_implemented_typed() {
        assert_round_trip(
            feature_not_implemented_iq_error("nope"),
            ErrorType::Cancel,
            DefinedCondition::FeatureNotImplemented,
        );
    }

    #[test]
    fn internal_server_error_typed() {
        assert_round_trip(
            internal_server_error_iq_error("nope"),
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
        );
    }

    /// Wire-shape assertion: the typed helper emits the same
    /// `<iq>/<error>/<defined-condition>` shape the previous
    /// stringly-typed builder did. The exact byte-for-byte serialisation
    /// (single vs. double quotes around `xmlns`, attribute ordering)
    /// is owned by the `xmpp_parsers`/`minidom` serialiser; we assert
    /// against its canonical output here so any future serialiser
    /// change is caught by this test rather than by every per-XEP
    /// integration test.
    #[test]
    fn wire_shape_matches_legacy_builder() {
        let typed = super::super::super::super::build_iq_error_xml_typed(
            "abc",
            Some("from@example"),
            Some("to@example"),
            StanzaError {
                type_: ErrorType::Cancel,
                by: None,
                defined_condition: DefinedCondition::ServiceUnavailable,
                texts: std::collections::BTreeMap::new(),
                other: None,
                alternate_address: None,
            },
        );
        let expected = "<iq xmlns='jabber:client' \
                        from=\"from@example\" id=\"abc\" \
                        to=\"to@example\" type=\"error\">\
                        <error type=\"cancel\">\
                        <service-unavailable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
                        </error></iq>";
        assert_eq!(typed, expected);
    }
}
