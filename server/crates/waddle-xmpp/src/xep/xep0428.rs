//! XEP-0428: Fallback Indication
//!
//! Marks part or all of a message's `<body/>` and/or `<subject/>` as fallback
//! content for a specific protocol (XEP-0461 replies, XEP-0424 message
//! retraction, XEP-0447 stateless file sharing, …). Capability-aware clients
//! strip the fallback when rendering the primary payload; legacy clients see
//! it as a regular body and gracefully degrade.
//!
//! Wire shape (XEP-0428 §2, v0.2.1):
//!
//! ```xml
//! <message to='anna@example.com' type='groupchat'>
//!   <body>&gt; Anna wrote:&#10;&gt; Hi&#10;Great</body>
//!   <reply to='anna@example.com' id='message-id1' xmlns='urn:xmpp:reply:0' />
//!   <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>
//!     <body start='0' end='33' />
//!   </fallback>
//! </message>
//! ```
//!
//! Key spec points (XEP-0428 §2.2):
//!
//! - The `for` attribute is OPTIONAL. When present, it names the specification
//!   the fallback substitutes for; when absent, the indication applies to all
//!   bodies and subjects of the message.
//! - `<body/>` and `<subject/>` children may carry optional `start`/`end`
//!   character offsets (XEP-0426 grapheme-aware UTF-16 code-unit positions —
//!   we treat them as plain UTF-16 code units per the JS string-slice model).
//! - A `<body/>` or `<subject/>` child with no offsets means the entire
//!   element is fallback.
//! - A `<fallback/>` with no children at all is shorthand for "every body and
//!   every subject in this message is fallback".

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0428 Fallback Indication.
pub const NS_FALLBACK: &str = "urn:xmpp:fallback:0";

/// A character range `[start, end)` inside a `<body/>` or `<subject/>` element,
/// measured in UTF-16 code units (matches XEP-0426's grapheme-position model
/// closely enough for the JS / browser substring slicing every existing
/// client uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackRange {
    pub start: usize,
    pub end: usize,
}

/// How a region (body or subject) participates in a fallback indication.
///
/// `Whole` corresponds to `<body/>` / `<subject/>` without `start`/`end`
/// attributes ("the entire element is fallback"). `Ranges` carries one or
/// more explicit range elements; the spec lets multiple per region appear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackRegion {
    /// The entire body or subject element is fallback content.
    Whole,
    /// Specific UTF-16 ranges within the body or subject are fallback.
    Ranges(Vec<FallbackRange>),
}

impl FallbackRegion {
    /// Build a region from an iterator of ranges. Empty input canonicalises
    /// to `Whole` (the same wire shape the spec uses for "no offsets given").
    pub fn from_ranges(ranges: impl IntoIterator<Item = FallbackRange>) -> Self {
        let collected: Vec<FallbackRange> = ranges.into_iter().collect();
        if collected.is_empty() {
            Self::Whole
        } else {
            Self::Ranges(collected)
        }
    }
}

/// A parsed `<fallback/>` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackIndication {
    /// `for=` attribute (XEP-0428 §2.2). Spec-optional; when `None`, the
    /// indication applies to all bodies and subjects of the message.
    pub for_ns: Option<String>,
    /// `<body/>` children, if any.
    pub body: Option<FallbackRegion>,
    /// `<subject/>` children, if any.
    pub subject: Option<FallbackRegion>,
}

impl FallbackIndication {
    /// `<fallback xmlns='urn:xmpp:fallback:0' for='X'><body/></fallback>` —
    /// the whole body is fallback for the named protocol.
    pub fn whole_body(for_ns: impl Into<String>) -> Self {
        Self {
            for_ns: Some(for_ns.into()),
            body: Some(FallbackRegion::Whole),
            subject: None,
        }
    }

    /// `<fallback for='X'><body start='S' end='E'/></fallback>` — a single
    /// UTF-16 range inside the body is fallback.
    pub fn for_range(for_ns: impl Into<String>, start: usize, end: usize) -> Self {
        Self::for_ranges(for_ns, [FallbackRange { start, end }])
    }

    /// `<fallback for='X'><body start='…' end='…'/>…</fallback>` — multiple
    /// UTF-16 ranges. An empty range list collapses to `whole_body`.
    pub fn for_ranges(
        for_ns: impl Into<String>,
        body_ranges: impl IntoIterator<Item = FallbackRange>,
    ) -> Self {
        Self {
            for_ns: Some(for_ns.into()),
            body: Some(FallbackRegion::from_ranges(body_ranges)),
            subject: None,
        }
    }

    /// `<fallback for='X'><subject/></fallback>` — the whole subject is
    /// fallback for the named protocol.
    pub fn whole_subject(for_ns: impl Into<String>) -> Self {
        Self {
            for_ns: Some(for_ns.into()),
            body: None,
            subject: Some(FallbackRegion::Whole),
        }
    }

    /// `<fallback/>` — no `for`, no children. Per the spec, this means
    /// every body and every subject in the carrier message is fallback.
    pub fn whole_message() -> Self {
        Self {
            for_ns: None,
            body: None,
            subject: None,
        }
    }
}

/// Check whether an element is an XEP-0428 `<fallback/>` payload.
pub fn is_fallback_element(elem: &Element) -> bool {
    elem.name() == "fallback" && elem.ns() == NS_FALLBACK
}

fn parse_usize_attr(elem: &Element, name: &str) -> Result<Option<usize>, ()> {
    match elem.attr(name).map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => value.parse::<usize>().map(Some).map_err(|_| ()),
        None => Ok(None),
    }
}

/// Parse a `<body/>` or `<subject/>` child of `<fallback/>`. Returns the
/// range, or `Whole` when no offsets are provided. `Err(())` flags a parse
/// failure (one-sided offsets, end-before-start, non-integer) so the caller
/// can reject the whole indication.
fn parse_region_child(elem: &Element) -> Result<FallbackRange, ()> {
    let start = parse_usize_attr(elem, "start")?;
    let end = parse_usize_attr(elem, "end")?;
    match (start, end) {
        (Some(start), Some(end)) if end >= start => Ok(FallbackRange { start, end }),
        (None, None) => Err(()), // caller distinguishes via separate path
        _ => Err(()),
    }
}

/// Collect every `<body/>` (or `<subject/>`, depending on `child_name`)
/// child of the `<fallback/>` element into a typed region.
///
/// Returns:
/// - `Ok(None)` if no children of that kind exist (the region is not
///   targeted by this indication).
/// - `Ok(Some(Whole))` if there is exactly one child with no offsets — the
///   spec's "the whole element is fallback" shape.
/// - `Ok(Some(Ranges([…])))` if all children declared offsets.
/// - `Err(())` if the element mixed offsets with no-offset children, or
///   carried malformed offsets — the indication is rejected.
fn collect_region(fallback_elem: &Element, child_name: &str) -> Result<Option<FallbackRegion>, ()> {
    let children: Vec<&Element> = fallback_elem
        .children()
        .filter(|child| child.name() == child_name && child.ns() == NS_FALLBACK)
        .collect();
    if children.is_empty() {
        return Ok(None);
    }
    // A single child with no offsets is the canonical "whole element"
    // form. Multiple children require every one of them to declare a range.
    if children.len() == 1 {
        let only = children[0];
        let start = parse_usize_attr(only, "start")?;
        let end = parse_usize_attr(only, "end")?;
        match (start, end) {
            (None, None) => return Ok(Some(FallbackRegion::Whole)),
            (Some(start), Some(end)) if end >= start => {
                return Ok(Some(FallbackRegion::Ranges(vec![FallbackRange {
                    start,
                    end,
                }])));
            }
            _ => return Err(()),
        }
    }
    let mut ranges = Vec::with_capacity(children.len());
    for child in children {
        ranges.push(parse_region_child(child)?);
    }
    Ok(Some(FallbackRegion::Ranges(ranges)))
}

/// Parse all `<fallback/>` payloads attached to a message.
///
/// Conformant inputs that are silently rejected:
/// - Malformed offsets (non-integer, end before start, half-supplied).
/// - Mixed "whole element" + range children in the same region.
///
/// `for=` is honoured as optional per the spec: a missing attribute leaves
/// `for_ns` as `None` and means the indication applies to every body and
/// subject in the message.
pub fn parse_fallbacks_from_message(msg: &Message) -> Vec<FallbackIndication> {
    msg.payloads
        .iter()
        .filter(|elem| is_fallback_element(elem))
        .filter_map(|elem| {
            let for_ns = elem
                .attr("for")
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned);
            let body = collect_region(elem, "body").ok()?;
            let subject = collect_region(elem, "subject").ok()?;
            Some(FallbackIndication {
                for_ns,
                body,
                subject,
            })
        })
        .collect()
}

fn append_region(parent: &mut Element, region: &FallbackRegion, name: &str) {
    match region {
        FallbackRegion::Whole => {
            parent.append_child(Element::builder(name, NS_FALLBACK).build());
        }
        FallbackRegion::Ranges(ranges) => {
            for range in ranges {
                parent.append_child(
                    Element::builder(name, NS_FALLBACK)
                        .attr(
                            minidom::rxml::xml_ncname!("start").to_owned(),
                            range.start.to_string(),
                        )
                        .attr(
                            minidom::rxml::xml_ncname!("end").to_owned(),
                            range.end.to_string(),
                        )
                        .build(),
                );
            }
        }
    }
}

/// Build an XEP-0428 `<fallback/>` element.
pub fn build_fallback_element(fallback: &FallbackIndication) -> Element {
    let mut builder = Element::builder("fallback", NS_FALLBACK);
    if let Some(for_ns) = fallback.for_ns.as_deref() {
        builder = builder.attr(minidom::rxml::xml_ncname!("for").to_owned(), for_ns);
    }
    let mut elem = builder.build();
    if let Some(subject) = &fallback.subject {
        append_region(&mut elem, subject, "subject");
    }
    if let Some(body) = &fallback.body {
        append_region(&mut elem, body, "body");
    }
    elem
}

/// Replace all fallback payloads on a message with the given set.
pub fn set_fallback_payloads(msg: &mut Message, fallbacks: &[FallbackIndication]) {
    msg.payloads.retain(|elem| !is_fallback_element(elem));
    for fallback in fallbacks {
        msg.payloads.push(build_fallback_element(fallback));
    }
}

/// Strip every fallback range from a body string, returning the
/// caller-visible text. Offsets are treated as UTF-16 code units per the
/// XEP-0426 character-position model the spec references; the body is
/// round-tripped through UTF-16 so emoji and other non-BMP characters are
/// stripped correctly.
pub fn strip_fallback_ranges(body: &str, ranges: &[FallbackRange]) -> String {
    if ranges.is_empty() {
        return body.to_string();
    }
    let units: Vec<u16> = body.encode_utf16().collect();
    let total = units.len();
    let mut keep = vec![true; total];
    for range in ranges {
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        for flag in keep.iter_mut().take(end).skip(start) {
            *flag = false;
        }
    }
    let kept: Vec<u16> = units
        .iter()
        .zip(keep.iter())
        .filter_map(|(u, k)| if *k { Some(*u) } else { None })
        .collect();
    String::from_utf16_lossy(&kept)
}
