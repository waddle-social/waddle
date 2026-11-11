//! Typed wire values for the extensions negotiated inline with SASL2:
//! XEP-0198 inline stream resumption and XEP-0386 Bind 2.
//!
//! Implemented against these revisions of the official `xsf/xeps` sources:
//!
//! - XEP-0388 (Extensible SASL Profile) 1.0.4, 2025-08-25: the `<inline/>`
//!   feature container, direct extension children of `<authenticate/>`, and
//!   inline results as direct children of `<success/>`.
//! - XEP-0198 (Stream Management) 1.6.3, 2025-07-28, section "SASL2 And BIND2
//!   Interaction": `<sm xmlns='urn:xmpp:sm:3'/>` inside the SASL2 `<inline/>`,
//!   `<resume h previd/>` inside `<authenticate/>`, `<resumed h previd/>` or
//!   `<failed [h]/>` inside `<success/>`, and `<enable/>` inside the Bind 2
//!   request answered by `<enabled/>` or `<failed/>` inside `<bound/>`.
//! - XEP-0386 (Bind 2) 1.1.0, 2025-12-12: `<bind xmlns='urn:xmpp:bind:0'>`
//!   with `<inline><feature var/></inline>` as the advertisement, `<bind>`
//!   with an optional `<tag/>` and other-namespace feature requests inside
//!   `<authenticate/>`, and `<bound/>` inside `<success/>`.
//!
//! The Bind 2 namespace and advertisement shape are taken from XEP-0386. The
//! examples in XEP-0198 1.6.3 spell the namespace `urn:xmpp:bind2:1`, list
//! `<feature/>` directly under `<bind/>` and show a `<latest-id/>` child of
//! `<bound/>`; none of those appear in the XEP-0386 text or schema, so they
//! are not modelled here.
//!
//! This module only parses and builds values. It neither advertises nor
//! processes inline requests in a server runtime.

use minidom::{Element, Node};
use thiserror::Error;
use xmpp_parsers::stanza_error::DefinedCondition;

use crate::disco::Feature;
use crate::xep0388::{Sasl2Authenticate, Sasl2InlineFeatures, Sasl2Success};

/// Stream Management namespace from XEP-0198.
pub const NS_SM: &str = "urn:xmpp:sm:3";
/// Bind 2 namespace from XEP-0386.
pub const NS_BIND2: &str = "urn:xmpp:bind:0";
/// RFC 6120 stanza error condition namespace used inside XEP-0198 `<failed/>`.
pub const NS_STANZA_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

/// Errors returned when parsing or combining inline SASL2 extension values.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum Sasl2InlineError {
    /// The element name or namespace does not match the expected value.
    #[error("invalid inline SASL2 extension element")]
    InvalidElement,
    /// `<inline/>` advertised XEP-0198 resumption more than once.
    #[error("duplicate inline stream-management feature")]
    DuplicateStreamManagementFeature,
    /// The advertised `<sm/>` feature carried attributes or content.
    #[error("invalid inline stream-management feature")]
    InvalidStreamManagementFeature,
    /// `<inline/>` advertised Bind 2 more than once.
    #[error("duplicate Bind 2 feature")]
    DuplicateBind2Feature,
    /// The advertised Bind 2 feature or its inline feature list was malformed.
    #[error("invalid Bind 2 feature")]
    InvalidBind2Feature,
    /// `<authenticate/>` carried more than one `<resume/>`.
    #[error("duplicate inline resume request")]
    DuplicateResumeRequest,
    /// The inline `<resume/>` had a missing or malformed `h` or `previd`.
    #[error("invalid inline resume request")]
    InvalidResumeRequest,
    /// `<authenticate/>` carried more than one Bind 2 `<bind/>`.
    #[error("duplicate Bind 2 request")]
    DuplicateBindRequest,
    /// The Bind 2 request had a duplicate tag, attributes or stray content.
    #[error("invalid Bind 2 request")]
    InvalidBindRequest,
    /// The XEP-0198 `<enable/>` inside a Bind 2 request was malformed or repeated.
    #[error("invalid inline stream-management enable request")]
    InvalidEnableRequest,
    /// `<success/>` carried more than one `<resumed/>` or `<failed/>`.
    #[error("duplicate inline resume result")]
    DuplicateResumeResult,
    /// The inline `<resumed/>` had a missing or malformed `h` or `previd`.
    #[error("invalid inline resumed result")]
    InvalidResumed,
    /// A `<failed/>` had a malformed `h` or an invalid error condition.
    #[error("invalid stream-management failure")]
    InvalidFailed,
    /// `<success/>` carried more than one `<bound/>`.
    #[error("duplicate Bind 2 result")]
    DuplicateBound,
    /// The `<bound/>` result had attributes or stray content.
    #[error("invalid Bind 2 result")]
    InvalidBound,
    /// `<bound/>` carried more than one `<enabled/>` or `<failed/>`.
    #[error("duplicate inline stream-management enable result")]
    DuplicateEnableResult,
    /// The `<enabled/>` inside `<bound/>` was malformed.
    #[error("invalid inline stream-management enabled result")]
    InvalidEnabled,
    /// `<success/>` reported both a resumed session and a newly bound one.
    #[error("Bind 2 result alongside a successful inline resumption")]
    BoundAfterResumption,
    /// A `urn:xmpp:sm:3` element appeared where XEP-0198 does not define it.
    #[error("unexpected stream-management element in an inline position")]
    UnexpectedStreamManagementElement,
    /// A `urn:xmpp:bind:0` element appeared where XEP-0386 does not define it.
    #[error("unexpected Bind 2 element in an inline position")]
    UnexpectedBind2Element,
}

/// Non-empty XEP-0198 stream-management session identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SmStreamId(String);

impl SmStreamId {
    /// Wrap a server-assigned stream-management identifier.
    ///
    /// Returns `None` for an empty identifier, which cannot name a session.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.is_empty()).then_some(Self(value))
    }

    /// Borrow the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// XEP-0198 `<resume/>` carried inside a SASL2 `<authenticate/>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmInlineResume {
    /// Identifier of the session to resume.
    pub previd: SmStreamId,
    /// Number of stanzas the client handled on that session, modulo 2^32.
    pub h: u32,
}

impl SmInlineResume {
    /// Parse an XEP-0198 `<resume/>` request.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2InlineError::InvalidElement`] for a different element and
    /// [`Sasl2InlineError::InvalidResumeRequest`] for content, unknown
    /// attributes, or a missing or malformed `h` or `previd`.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2InlineError> {
        if !element.is("resume", NS_SM) {
            return Err(Sasl2InlineError::InvalidElement);
        }
        let (previd, h) =
            parse_resumption(element).ok_or(Sasl2InlineError::InvalidResumeRequest)?;
        Ok(Self { previd, h })
    }

    /// Build an XEP-0198 `<resume/>` request.
    #[must_use]
    pub fn to_element(&self) -> Element {
        build_resumption("resume", &self.previd, self.h)
    }
}

/// XEP-0198 `<resumed/>` carried inside a SASL2 `<success/>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmInlineResumed {
    /// Identifier of the resumed session.
    pub previd: SmStreamId,
    /// Number of stanzas the server handled on that session, modulo 2^32.
    pub h: u32,
}

impl SmInlineResumed {
    /// Parse an XEP-0198 `<resumed/>` result.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2InlineError::InvalidElement`] for a different element and
    /// [`Sasl2InlineError::InvalidResumed`] for content, unknown attributes,
    /// or a missing or malformed `h` or `previd`.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2InlineError> {
        if !element.is("resumed", NS_SM) {
            return Err(Sasl2InlineError::InvalidElement);
        }
        let (previd, h) = parse_resumption(element).ok_or(Sasl2InlineError::InvalidResumed)?;
        Ok(Self { previd, h })
    }

    /// Build an XEP-0198 `<resumed/>` result.
    #[must_use]
    pub fn to_element(&self) -> Element {
        build_resumption("resumed", &self.previd, self.h)
    }
}

/// XEP-0198 `<failed/>` answering an inline `<resume/>` or `<enable/>`.
#[derive(Debug, Clone, PartialEq)]
pub struct SmInlineFailed {
    /// Number of stanzas the server handled on the session that could not be
    /// resumed, when the server still knows it.
    pub h: Option<u32>,
    /// RFC 6120 stanza error condition explaining the failure.
    pub condition: Option<DefinedCondition>,
}

impl SmInlineFailed {
    /// Parse an XEP-0198 `<failed/>` result.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2InlineError::InvalidElement`] for a different element and
    /// [`Sasl2InlineError::InvalidFailed`] for unknown attributes, a malformed
    /// `h`, text content, more than one child, or a child that is not an
    /// RFC 6120 stanza error condition.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2InlineError> {
        if !element.is("failed", NS_SM) {
            return Err(Sasl2InlineError::InvalidElement);
        }
        Self::parse(element).ok_or(Sasl2InlineError::InvalidFailed)
    }

    fn parse(element: &Element) -> Option<Self> {
        if !has_only_attributes(element, &["h"]) {
            return None;
        }
        let h = match element.attr("h") {
            Some(value) => Some(parse_unsigned_int(value)?),
            None => None,
        };
        let mut condition = None;
        for child in element_children(element)? {
            if condition.is_some() || child.ns() != NS_STANZA_ERRORS {
                return None;
            }
            condition = Some(DefinedCondition::try_from(child.clone()).ok()?);
        }
        Some(Self { h, condition })
    }

    /// Build an XEP-0198 `<failed/>` result.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("failed", NS_SM);
        if let Some(h) = self.h {
            builder = builder.attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string());
        }
        if let Some(condition) = &self.condition {
            builder = builder.append(Element::from(condition.clone()));
        }
        builder.build()
    }
}

/// XEP-0198 `<enable/>` carried inside a Bind 2 request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SmInlineEnable {
    /// Whether the client asks for the new session to be resumable.
    pub resume: bool,
    /// Preferred maximum resumption time in seconds.
    pub max: Option<u32>,
}

impl SmInlineEnable {
    /// Parse an XEP-0198 `<enable/>` request.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2InlineError::InvalidElement`] for a different element and
    /// [`Sasl2InlineError::InvalidEnableRequest`] for content, unknown
    /// attributes, a non-boolean `resume`, or a non-positive `max`.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2InlineError> {
        if !element.is("enable", NS_SM) {
            return Err(Sasl2InlineError::InvalidElement);
        }
        Self::parse(element).ok_or(Sasl2InlineError::InvalidEnableRequest)
    }

    fn parse(element: &Element) -> Option<Self> {
        if !is_empty(element) || !has_only_attributes(element, &["max", "resume"]) {
            return None;
        }
        let resume = match element.attr("resume") {
            Some(value) => parse_boolean(value)?,
            None => false,
        };
        let max = match element.attr("max") {
            Some(value) => Some(parse_positive_int(value)?),
            None => None,
        };
        Some(Self { resume, max })
    }

    /// Build an XEP-0198 `<enable/>` request.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("enable", NS_SM);
        if self.resume {
            builder = builder.attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true");
        }
        if let Some(max) = self.max {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("max").to_owned(),
                max.to_string(),
            );
        }
        builder.build()
    }
}

/// XEP-0198 `<enabled/>` carried inside a Bind 2 `<bound/>` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmInlineEnabled {
    /// Identifier of the new session; always present when it is resumable.
    pub id: Option<SmStreamId>,
    /// Whether the server allows the new session to be resumed.
    pub resume: bool,
    /// Server's maximum resumption time in seconds.
    pub max: Option<u32>,
    /// Server's preferred reconnection location for resumption.
    pub location: Option<String>,
}

impl SmInlineEnabled {
    /// Parse an XEP-0198 `<enabled/>` result.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2InlineError::InvalidElement`] for a different element and
    /// [`Sasl2InlineError::InvalidEnabled`] for content, unknown attributes, a
    /// non-boolean `resume`, a non-positive `max`, an empty `id`, or a
    /// resumable session without an `id`.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2InlineError> {
        if !element.is("enabled", NS_SM) {
            return Err(Sasl2InlineError::InvalidElement);
        }
        Self::parse(element).ok_or(Sasl2InlineError::InvalidEnabled)
    }

    fn parse(element: &Element) -> Option<Self> {
        if !is_empty(element) || !has_only_attributes(element, &["id", "location", "max", "resume"])
        {
            return None;
        }
        let resume = match element.attr("resume") {
            Some(value) => parse_boolean(value)?,
            None => false,
        };
        let max = match element.attr("max") {
            Some(value) => Some(parse_positive_int(value)?),
            None => None,
        };
        let id = match element.attr("id") {
            Some(value) => Some(SmStreamId::new(value)?),
            None => None,
        };
        if resume && id.is_none() {
            return None;
        }
        Some(Self {
            id,
            resume,
            max,
            location: element.attr("location").map(str::to_owned),
        })
    }

    /// Build an XEP-0198 `<enabled/>` result.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("enabled", NS_SM);
        if let Some(id) = &self.id {
            builder = builder.attr(minidom::rxml::xml_ncname!("id").to_owned(), id.as_str());
        }
        if self.resume {
            builder = builder.attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true");
        }
        if let Some(max) = self.max {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("max").to_owned(),
                max.to_string(),
            );
        }
        if let Some(location) = &self.location {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("location").to_owned(),
                location.as_str(),
            );
        }
        builder.build()
    }
}

/// XEP-0386 Bind 2 feature advertised inside the SASL2 `<inline/>` element.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bind2Feature {
    /// Features the server can negotiate inline with the bind, in wire order.
    pub inline_features: Vec<Feature>,
}

impl Bind2Feature {
    /// Whether XEP-0198 can be enabled inline with the bind.
    #[must_use]
    pub fn supports_stream_management(&self) -> bool {
        self.inline_features
            .iter()
            .any(|feature| feature.0 == NS_SM)
    }

    /// Parse an XEP-0386 `<bind/>` feature advertisement.
    ///
    /// Children in other namespaces are ignored; a repeated `var` is listed
    /// once.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2InlineError::InvalidElement`] for a different element and
    /// [`Sasl2InlineError::InvalidBind2Feature`] for attributes, text, a
    /// repeated `<inline/>`, a Bind 2 child other than `<inline/>`, or an
    /// inline entry that is not an empty `<feature var='…'/>`.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2InlineError> {
        if !element.is("bind", NS_BIND2) {
            return Err(Sasl2InlineError::InvalidElement);
        }
        Self::parse(element).ok_or(Sasl2InlineError::InvalidBind2Feature)
    }

    fn parse(element: &Element) -> Option<Self> {
        if !element.attrs().is_empty() {
            return None;
        }
        let mut inline = None;
        for child in element_children(element)? {
            if child.ns() != NS_BIND2 {
                continue;
            }
            if child.name() != "inline" || inline.is_some() || !child.attrs().is_empty() {
                return None;
            }
            let mut features: Vec<Feature> = Vec::new();
            for entry in element_children(child)? {
                if !entry.is("feature", NS_BIND2)
                    || !is_empty(entry)
                    || !has_only_attributes(entry, &["var"])
                {
                    return None;
                }
                let var = entry.attr("var").filter(|var| !var.is_empty())?;
                if !features.iter().any(|feature| feature.0 == var) {
                    features.push(Feature::new(var));
                }
            }
            inline = Some(features);
        }
        Some(Self {
            inline_features: inline.unwrap_or_default(),
        })
    }

    /// Build an XEP-0386 `<bind/>` feature advertisement.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("bind", NS_BIND2);
        if !self.inline_features.is_empty() {
            let mut inline = Element::builder("inline", NS_BIND2);
            for feature in &self.inline_features {
                inline = inline.append(
                    Element::builder("feature", NS_BIND2)
                        .attr(
                            minidom::rxml::xml_ncname!("var").to_owned(),
                            feature.0.as_str(),
                        )
                        .build(),
                );
            }
            builder = builder.append(inline.build());
        }
        builder.build()
    }
}

/// Inline features this module understands, from the SASL2 `<inline/>` element.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sasl2InlineOffer {
    /// Whether XEP-0198 resumption can be requested inside `<authenticate/>`.
    pub stream_resumption: bool,
    /// Bind 2 support and the features it can negotiate inline.
    pub bind2: Option<Bind2Feature>,
}

impl Sasl2InlineOffer {
    /// Interpret the feature elements of a SASL2 `<inline/>` container.
    ///
    /// Features in other namespaces are ignored.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a repeated or malformed `<sm/>` or Bind 2
    /// feature, or for any other element in either namespace.
    pub fn from_inline_features(inline: &Sasl2InlineFeatures) -> Result<Self, Sasl2InlineError> {
        let mut offer = Self::default();
        for feature in &inline.features {
            match (feature.ns().as_str(), feature.name()) {
                (NS_SM, "sm") => {
                    if offer.stream_resumption {
                        return Err(Sasl2InlineError::DuplicateStreamManagementFeature);
                    }
                    if !is_empty(feature) || !feature.attrs().is_empty() {
                        return Err(Sasl2InlineError::InvalidStreamManagementFeature);
                    }
                    offer.stream_resumption = true;
                }
                (NS_SM, _) => return Err(Sasl2InlineError::UnexpectedStreamManagementElement),
                (NS_BIND2, "bind") => {
                    if offer.bind2.is_some() {
                        return Err(Sasl2InlineError::DuplicateBind2Feature);
                    }
                    offer.bind2 = Some(Bind2Feature::from_element(feature)?);
                }
                (NS_BIND2, _) => return Err(Sasl2InlineError::UnexpectedBind2Element),
                _ => {}
            }
        }
        Ok(offer)
    }

    /// Build the SASL2 `<inline/>` container advertising exactly this offer.
    #[must_use]
    pub fn to_inline_features(&self) -> Sasl2InlineFeatures {
        let mut features = Vec::new();
        if self.stream_resumption {
            features.push(Element::builder("sm", NS_SM).build());
        }
        if let Some(bind2) = &self.bind2 {
            features.push(bind2.to_element());
        }
        Sasl2InlineFeatures { features }
    }
}

/// Client software tag carried in a Bind 2 request.
///
/// The server may embed it in the generated resource, where other entities
/// can see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind2Tag(String);

impl Bind2Tag {
    /// Wrap the tag text.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the tag text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// XEP-0386 Bind 2 request carried inside a SASL2 `<authenticate/>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bind2Request {
    /// Optional client software tag.
    pub tag: Option<Bind2Tag>,
    /// XEP-0198 enable request for the newly bound session.
    pub stream_management: Option<SmInlineEnable>,
    /// Feature requests in namespaces this module does not model.
    pub other_features: Vec<Element>,
}

impl Bind2Request {
    /// Parse an XEP-0386 `<bind/>` request.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2InlineError::InvalidElement`] for a different element,
    /// [`Sasl2InlineError::InvalidBindRequest`] for attributes, text, a
    /// repeated or non-text `<tag/>`, or any other Bind 2 child, and
    /// [`Sasl2InlineError::InvalidEnableRequest`] for a malformed or repeated
    /// XEP-0198 `<enable/>`.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2InlineError> {
        if !element.is("bind", NS_BIND2) {
            return Err(Sasl2InlineError::InvalidElement);
        }
        if !element.attrs().is_empty() {
            return Err(Sasl2InlineError::InvalidBindRequest);
        }
        let mut request = Self::default();
        for child in element_children(element).ok_or(Sasl2InlineError::InvalidBindRequest)? {
            match (child.ns().as_str(), child.name()) {
                (NS_BIND2, "tag") => {
                    if request.tag.is_some()
                        || !child.attrs().is_empty()
                        || child.children().next().is_some()
                    {
                        return Err(Sasl2InlineError::InvalidBindRequest);
                    }
                    request.tag = Some(Bind2Tag::new(child.text()));
                }
                (NS_BIND2, _) => return Err(Sasl2InlineError::InvalidBindRequest),
                (NS_SM, "enable") => {
                    if request.stream_management.is_some() {
                        return Err(Sasl2InlineError::InvalidEnableRequest);
                    }
                    request.stream_management = Some(SmInlineEnable::from_element(child)?);
                }
                (NS_SM, _) => return Err(Sasl2InlineError::UnexpectedStreamManagementElement),
                _ => request.other_features.push(child.clone()),
            }
        }
        Ok(request)
    }

    /// Build an XEP-0386 `<bind/>` request.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("bind", NS_BIND2);
        if let Some(tag) = &self.tag {
            builder = builder.append(
                Element::builder("tag", NS_BIND2)
                    .append(tag.as_str())
                    .build(),
            );
        }
        if let Some(enable) = &self.stream_management {
            builder = builder.append(enable.to_element());
        }
        for feature in &self.other_features {
            builder = builder.append(feature.clone());
        }
        builder.build()
    }
}

/// Inline extension requests carried as direct children of `<authenticate/>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sasl2InlineRequests {
    /// XEP-0198 resumption of a previous session.
    pub resume: Option<SmInlineResume>,
    /// XEP-0386 resource binding, processed only when no session is resumed.
    pub bind: Option<Bind2Request>,
}

impl Sasl2InlineRequests {
    /// Whether no inline extension is requested.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resume.is_none() && self.bind.is_none()
    }

    /// Interpret the extension children of a SASL2 `<authenticate/>` request.
    ///
    /// Children in other namespaces, including the SASL2 `<user-agent/>`, are
    /// ignored.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a repeated or malformed `<resume/>` or Bind 2
    /// request, or for any other element in either namespace.
    pub fn from_authenticate(request: &Sasl2Authenticate) -> Result<Self, Sasl2InlineError> {
        let mut requests = Self::default();
        for child in request.additional_children() {
            match (child.ns().as_str(), child.name()) {
                (NS_SM, "resume") => {
                    if requests.resume.is_some() {
                        return Err(Sasl2InlineError::DuplicateResumeRequest);
                    }
                    requests.resume = Some(SmInlineResume::from_element(child)?);
                }
                (NS_SM, _) => return Err(Sasl2InlineError::UnexpectedStreamManagementElement),
                (NS_BIND2, "bind") => {
                    if requests.bind.is_some() {
                        return Err(Sasl2InlineError::DuplicateBindRequest);
                    }
                    requests.bind = Some(Bind2Request::from_element(child)?);
                }
                (NS_BIND2, _) => return Err(Sasl2InlineError::UnexpectedBind2Element),
                _ => {}
            }
        }
        Ok(requests)
    }

    /// Build the request elements in wire order: resumption, then the bind
    /// that applies only when no session is resumed.
    #[must_use]
    pub fn to_elements(&self) -> Vec<Element> {
        self.resume
            .iter()
            .map(SmInlineResume::to_element)
            .chain(self.bind.iter().map(Bind2Request::to_element))
            .collect()
    }

    /// Append these requests to a SASL2 `<authenticate/>` request, resumption
    /// first.
    ///
    /// # Errors
    ///
    /// Returns a typed error, leaving `request` unchanged, when it already
    /// carries a resumption or Bind 2 request that this value would repeat,
    /// or when its existing extension children are malformed.
    pub fn append_to(&self, request: &mut Sasl2Authenticate) -> Result<(), Sasl2InlineError> {
        let existing = Self::from_authenticate(request)?;
        if self.resume.is_some() && existing.resume.is_some() {
            return Err(Sasl2InlineError::DuplicateResumeRequest);
        }
        if self.bind.is_some() && existing.bind.is_some() {
            return Err(Sasl2InlineError::DuplicateBindRequest);
        }
        for child in self.to_elements() {
            request
                .push_additional_child(child)
                .map_err(|_| Sasl2InlineError::InvalidElement)?;
        }
        Ok(())
    }
}

/// Outcome of an inline XEP-0198 `<resume/>` request.
#[derive(Debug, Clone, PartialEq)]
pub enum SmInlineResumeResult {
    /// The previous session continues on this stream.
    Resumed(SmInlineResumed),
    /// The previous session could not be resumed.
    Failed(SmInlineFailed),
}

/// Outcome of an XEP-0198 `<enable/>` requested inside a Bind 2 request.
#[derive(Debug, Clone, PartialEq)]
pub enum Bind2StreamManagementResult {
    /// Stream management is active on the newly bound session.
    Enabled(SmInlineEnabled),
    /// The resource was bound but stream management could not be enabled.
    Failed(SmInlineFailed),
}

/// XEP-0386 `<bound/>` result carried inside a SASL2 `<success/>`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bind2Bound {
    /// Result of the XEP-0198 enable request sent with the bind, if any.
    pub stream_management: Option<Bind2StreamManagementResult>,
    /// Results in namespaces this module does not model, such as the
    /// XEP-0313 archive metadata.
    pub other_results: Vec<Element>,
}

impl Bind2Bound {
    /// Parse an XEP-0386 `<bound/>` result.
    ///
    /// # Errors
    ///
    /// Returns [`Sasl2InlineError::InvalidElement`] for a different element,
    /// [`Sasl2InlineError::InvalidBound`] for attributes, text, or a Bind 2
    /// child, [`Sasl2InlineError::DuplicateEnableResult`] for more than one
    /// XEP-0198 result, and a typed error for a malformed `<enabled/>` or
    /// `<failed/>` or any other XEP-0198 element.
    pub fn from_element(element: &Element) -> Result<Self, Sasl2InlineError> {
        if !element.is("bound", NS_BIND2) {
            return Err(Sasl2InlineError::InvalidElement);
        }
        if !element.attrs().is_empty() {
            return Err(Sasl2InlineError::InvalidBound);
        }
        let mut bound = Self::default();
        for child in element_children(element).ok_or(Sasl2InlineError::InvalidBound)? {
            match (child.ns().as_str(), child.name()) {
                (NS_SM, "enabled" | "failed") => {
                    if bound.stream_management.is_some() {
                        return Err(Sasl2InlineError::DuplicateEnableResult);
                    }
                    bound.stream_management = Some(if child.name() == "enabled" {
                        Bind2StreamManagementResult::Enabled(SmInlineEnabled::from_element(child)?)
                    } else {
                        Bind2StreamManagementResult::Failed(SmInlineFailed::from_element(child)?)
                    });
                }
                (NS_SM, _) => return Err(Sasl2InlineError::UnexpectedStreamManagementElement),
                (NS_BIND2, _) => return Err(Sasl2InlineError::InvalidBound),
                _ => bound.other_results.push(child.clone()),
            }
        }
        Ok(bound)
    }

    /// Build an XEP-0386 `<bound/>` result.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("bound", NS_BIND2);
        match &self.stream_management {
            Some(Bind2StreamManagementResult::Enabled(enabled)) => {
                builder = builder.append(enabled.to_element());
            }
            Some(Bind2StreamManagementResult::Failed(failed)) => {
                builder = builder.append(failed.to_element());
            }
            None => {}
        }
        for result in &self.other_results {
            builder = builder.append(result.clone());
        }
        builder.build()
    }
}

/// Inline extension results carried as direct children of `<success/>`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sasl2InlineResults {
    /// Outcome of the inline XEP-0198 resumption request.
    pub resume: Option<SmInlineResumeResult>,
    /// Newly bound session, present only when no session was resumed.
    pub bound: Option<Bind2Bound>,
    /// Results in namespaces this module does not model.
    pub other_results: Vec<Element>,
}

impl Sasl2InlineResults {
    /// Interpret the inline results of a SASL2 `<success/>` response.
    ///
    /// # Errors
    ///
    /// See [`Self::from_elements`].
    pub fn from_success(success: &Sasl2Success) -> Result<Self, Sasl2InlineError> {
        Self::from_elements(&success.inline_results)
    }

    /// Interpret inline result elements.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a repeated or malformed resumption or Bind 2
    /// result, any other element in either namespace, or a `<bound/>` result
    /// alongside `<resumed/>`: a resumed session keeps its existing resource,
    /// so XEP-0386 requires the server to skip the bind.
    pub fn from_elements(elements: &[Element]) -> Result<Self, Sasl2InlineError> {
        let mut results = Self::default();
        for element in elements {
            match (element.ns().as_str(), element.name()) {
                (NS_SM, "resumed" | "failed") => {
                    if results.resume.is_some() {
                        return Err(Sasl2InlineError::DuplicateResumeResult);
                    }
                    results.resume = Some(if element.name() == "resumed" {
                        SmInlineResumeResult::Resumed(SmInlineResumed::from_element(element)?)
                    } else {
                        SmInlineResumeResult::Failed(SmInlineFailed::from_element(element)?)
                    });
                }
                (NS_SM, _) => return Err(Sasl2InlineError::UnexpectedStreamManagementElement),
                (NS_BIND2, "bound") => {
                    if results.bound.is_some() {
                        return Err(Sasl2InlineError::DuplicateBound);
                    }
                    results.bound = Some(Bind2Bound::from_element(element)?);
                }
                (NS_BIND2, _) => return Err(Sasl2InlineError::UnexpectedBind2Element),
                _ => results.other_results.push(element.clone()),
            }
        }
        if matches!(results.resume, Some(SmInlineResumeResult::Resumed(_)))
            && results.bound.is_some()
        {
            return Err(Sasl2InlineError::BoundAfterResumption);
        }
        Ok(results)
    }

    /// Build the inline result elements: resumption outcome, then `<bound/>`,
    /// then unmodelled results.
    #[must_use]
    pub fn to_elements(&self) -> Vec<Element> {
        let mut elements = Vec::new();
        match &self.resume {
            Some(SmInlineResumeResult::Resumed(resumed)) => elements.push(resumed.to_element()),
            Some(SmInlineResumeResult::Failed(failed)) => elements.push(failed.to_element()),
            None => {}
        }
        if let Some(bound) = &self.bound {
            elements.push(bound.to_element());
        }
        elements.extend(self.other_results.iter().cloned());
        elements
    }
}

fn parse_resumption(element: &Element) -> Option<(SmStreamId, u32)> {
    if !is_empty(element) || !has_only_attributes(element, &["h", "previd"]) {
        return None;
    }
    let h = parse_unsigned_int(element.attr("h")?)?;
    let previd = SmStreamId::new(element.attr("previd")?)?;
    Some((previd, h))
}

fn build_resumption(name: &'static str, previd: &SmStreamId, h: u32) -> Element {
    Element::builder(name, NS_SM)
        .attr(
            minidom::rxml::xml_ncname!("previd").to_owned(),
            previd.as_str(),
        )
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string())
        .build()
}

/// The element has no child elements and no text, matching the XEP-0198
/// schema's `empty` content type.
fn is_empty(element: &Element) -> bool {
    element.nodes().next().is_none()
}

fn has_only_attributes(element: &Element, allowed: &[&str]) -> bool {
    element
        .attrs()
        .iter()
        .all(|((namespace, name), _)| namespace.is_empty() && allowed.contains(&name.as_str()))
}

/// Child elements of an element-only container, or `None` when it also holds
/// text other than XML whitespace.
fn element_children(element: &Element) -> Option<Vec<&Element>> {
    let mut children = Vec::new();
    for node in element.nodes() {
        match node {
            Node::Element(child) => children.push(child),
            Node::Text(text) => {
                if !text.chars().all(is_xml_whitespace) {
                    return None;
                }
            }
        }
    }
    Some(children)
}

fn is_xml_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r')
}

/// Parse an `xs:unsignedInt` lexical value.
fn parse_unsigned_int(value: &str) -> Option<u32> {
    let value = value.trim_matches(is_xml_whitespace);
    let digits = value.strip_prefix('+').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Parse an `xs:positiveInteger` lexical value that fits in 32 bits.
fn parse_positive_int(value: &str) -> Option<u32> {
    parse_unsigned_int(value).filter(|value| *value != 0)
}

/// Parse an `xs:boolean` lexical value.
fn parse_boolean(value: &str) -> Option<bool> {
    match value.trim_matches(is_xml_whitespace) {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}
