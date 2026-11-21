//! Negotiation elements sent by this codec.

use minidom::rxml::xml_ncname;
use minidom::Element;
use xmpp_parsers::sasl::{Challenge, Success};
use xmpp_parsers::starttls::{Proceed, Request, StartTls};

use crate::ns::{SASL, STREAM};

/// The SASL EXTERNAL mechanism name (RFC 4422 Appendix A).
pub(crate) const EXTERNAL: &str = "EXTERNAL";

/// The zero-length initial response marker (RFC 6120 §6.4.2).
const EMPTY_RESPONSE: &str = "=";

/// Pre-TLS features: STARTTLS only, marked mandatory-to-negotiate
/// (RFC 6120 §5.3.1).
pub(crate) fn starttls_features() -> Element {
    Element::builder("features", STREAM)
        .append(Element::from(StartTls { required: true }))
        .build()
}

/// Post-TLS features: SASL EXTERNAL only.
pub(crate) fn external_features() -> Element {
    let mechanism = Element::builder("mechanism", SASL).append(EXTERNAL).build();
    let mechanisms = Element::builder("mechanisms", SASL)
        .append(mechanism)
        .build();
    Element::builder("features", STREAM)
        .append(mechanisms)
        .build()
}

/// Post-SASL features: an empty element, as nothing further is offered
/// (RFC 6120 §4.3.5).
pub(crate) fn empty_features() -> Element {
    Element::builder("features", STREAM).build()
}

pub(crate) fn starttls_request() -> Element {
    Element::from(Request)
}

pub(crate) fn proceed() -> Element {
    Element::from(Proceed)
}

/// `<auth mechanism='EXTERNAL'>=</auth>`: authenticate as the
/// certificate-verified source domain without an authorization identity
/// (RFC 6120 §6.3.8).
pub(crate) fn external_auth() -> Element {
    Element::builder("auth", SASL)
        .attr(xml_ncname!("mechanism").to_owned(), EXTERNAL)
        .append(EMPTY_RESPONSE)
        .build()
}

/// An empty challenge requesting the omitted initial response.
pub(crate) fn empty_challenge() -> Element {
    Element::from(Challenge { data: Vec::new() })
}

pub(crate) fn success() -> Element {
    Element::from(Success { data: Vec::new() })
}
