//! Server Dialback (XEP-0220) elements: reading what a peer sent and
//! building what this codec sends.
//!
//! Received `from` and `to` attributes are read like stream header
//! addresses ([`HeaderAddress::parse`]), into the identity profile's
//! canonical form, so they compare with the stream's pair in that form.
//! Emitted addresses are U-labels, as on stream headers. Emitted elements
//! are qualified by `jabber:server:dialback`, which the stream encoder
//! writes with the `db` prefix the stream header declared.

use minidom::rxml::xml_ncname;
use minidom::{Element, ElementBuilder};

use crate::identity::CanonicalDomain;
use crate::ns::{DIALBACK_FEATURES, JABBER_SERVER, JABBER_SERVER_DIALBACK, STANZAS};
use crate::s2s::header::{HeaderAddress, StreamId};

use super::types::{DialbackError, DialbackKey, DialbackOutcome, VerifyVerdict};

/// The `type` attribute of a dialback element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialbackType {
    /// No `type`: a request (step 1 or step 2 of XEP-0220).
    Request,
    Valid,
    Invalid,
    Error,
    /// Any other value.
    Unknown,
}

impl DialbackType {
    fn read(element: &Element) -> Self {
        match element.attr("type") {
            None => Self::Request,
            Some("valid") => Self::Valid,
            Some("invalid") => Self::Invalid,
            Some("error") => Self::Error,
            Some(_) => Self::Unknown,
        }
    }
}

/// A received first-level element in the dialback namespace.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DialbackElement {
    /// `<db:result/>`.
    Result {
        from: HeaderAddress,
        to: HeaderAddress,
        kind: DialbackType,
        /// The key of a request; `None` when absent or not a valid key.
        key: Option<DialbackKey>,
    },
    /// `<db:verify/>`.
    Verify {
        from: HeaderAddress,
        to: HeaderAddress,
        id: Option<StreamId>,
        kind: DialbackType,
        /// The key of a request; `None` when absent or not a valid key.
        key: Option<DialbackKey>,
    },
    /// Any other element in the dialback namespace.
    Other,
}

/// Read a first-level element qualified by `jabber:server:dialback`.
pub(crate) fn read(element: &Element) -> DialbackElement {
    let from = HeaderAddress::parse(element.attr("from"));
    let to = HeaderAddress::parse(element.attr("to"));
    let kind = DialbackType::read(element);
    // A key is character data only; an element with children carries none.
    let key = if element.children().next().is_none() {
        DialbackKey::parse(&element.text())
    } else {
        None
    };
    match element.name() {
        "result" => DialbackElement::Result {
            from,
            to,
            kind,
            key,
        },
        "verify" => DialbackElement::Verify {
            from,
            to,
            id: element.attr("id").and_then(StreamId::parse),
            kind,
            key,
        },
        _ => DialbackElement::Other,
    }
}

fn builder(name: &str, from: &CanonicalDomain, to: &CanonicalDomain) -> ElementBuilder {
    Element::builder(name, JABBER_SERVER_DIALBACK)
        .attr(xml_ncname!("from").to_owned(), from.to_unicode())
        .attr(xml_ncname!("to").to_owned(), to.to_unicode())
}

/// `<db:result from='originating' to='receiving'>key</db:result>`: the
/// Initiating Server's key (XEP-0220 §2.1.1).
pub(crate) fn result_request(
    from: &CanonicalDomain,
    to: &CanonicalDomain,
    key: &DialbackKey,
) -> Element {
    builder("result", from, to).append(key.as_str()).build()
}

/// The Receiving Server's `<db:result/>` answer to the Initiating Server
/// (XEP-0220 §2.2.1): `from` is the Target Domain and `to` the Sender
/// Domain.
pub(crate) fn result_answer(
    from: &CanonicalDomain,
    to: &CanonicalDomain,
    answer: DialbackOutcome,
) -> Element {
    let builder = builder("result", from, to);
    match answer {
        DialbackOutcome::Valid => builder.attr(xml_ncname!("type").to_owned(), "valid"),
        DialbackOutcome::Invalid => builder.attr(xml_ncname!("type").to_owned(), "invalid"),
        DialbackOutcome::Error(error) => builder
            .attr(xml_ncname!("type").to_owned(), "error")
            .append(dialback_error(error)),
    }
    .build()
}

/// `<error type='cancel'><condition/></error>` in `jabber:server`, the
/// content of a dialback error (XEP-0220 §2.5).
fn dialback_error(error: DialbackError) -> Element {
    Element::builder("error", JABBER_SERVER)
        .attr(xml_ncname!("type").to_owned(), "cancel")
        .append(Element::builder(error.condition(), STANZAS).build())
        .build()
}

/// `<db:verify from='receiving' to='originating' id='…'>key</db:verify>`:
/// the Receiving Server's verification request (XEP-0220 §2.1.2).
pub(crate) fn verify_request(
    from: &CanonicalDomain,
    to: &CanonicalDomain,
    id: &StreamId,
    key: &DialbackKey,
) -> Element {
    builder("verify", from, to)
        .attr(xml_ncname!("id").to_owned(), id.as_str())
        .append(key.as_str())
        .build()
}

/// The Authoritative Server's answer (XEP-0220 §2.2.2): `from` and `to`
/// swapped from the request and its `id` copied.
pub(crate) fn verify_answer(
    from: &CanonicalDomain,
    to: &CanonicalDomain,
    id: &StreamId,
    valid: bool,
) -> Element {
    builder("verify", from, to)
        .attr(xml_ncname!("id").to_owned(), id.as_str())
        .attr(
            xml_ncname!("type").to_owned(),
            if valid { "valid" } else { "invalid" },
        )
        .build()
}

/// The `<dialback xmlns='urn:xmpp:features:dialback'><errors/></dialback>`
/// stream feature: dialback with error handling (XEP-0220 §2.4.2).
pub(crate) fn feature() -> Element {
    Element::builder("dialback", DIALBACK_FEATURES)
        .append(Element::builder("errors", DIALBACK_FEATURES).build())
        .build()
}

/// The verdict a `type` attribute carries on an answer, if it is one.
pub(crate) fn verdict(kind: DialbackType) -> Option<VerifyVerdict> {
    match kind {
        DialbackType::Valid => Some(VerifyVerdict::Valid),
        DialbackType::Invalid => Some(VerifyVerdict::Invalid),
        DialbackType::Error => Some(VerifyVerdict::Error),
        DialbackType::Request | DialbackType::Unknown => None,
    }
}

/// The Authoritative Server's dialback error for a verification request
/// (XEP-0220 §2.2.2 and §2.5): `from` and `to` swapped from the request and
/// its `id` copied. The stream stays open.
pub(crate) fn verify_error(
    from: &CanonicalDomain,
    to: &CanonicalDomain,
    id: &StreamId,
    error: DialbackError,
) -> Element {
    builder("verify", from, to)
        .attr(xml_ncname!("id").to_owned(), id.as_str())
        .attr(xml_ncname!("type").to_owned(), "error")
        .append(dialback_error(error))
        .build()
}
