//! Typed XML wire shapes for XEP-0397 (ADR-0017 Phase 3 Slice 8).
//!
//! Every element here is built/parsed with `minidom::Element`, never
//! `format!`/string concatenation (the repo's XML-generation hard rule).

use minidom::Element;

use crate::stream_management::{SmFailed, SmResume, SmResumed, SM_NS};

use super::ISR_NS;

/// `<isr-enable mechanism='...'/>`, parsed out of an inline child of
/// `<enable/>` (XEP-0397 "Obtaining a Instant Stream Resumption Token").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsrEnable {
    pub mechanism: String,
}

impl IsrEnable {
    /// Look for an `<isr-enable/>` child (qualified by [`ISR_NS`]) inside
    /// `parent` (the `<enable/>` element) and parse it, if present.
    pub fn from_element(parent: &Element) -> Option<Self> {
        let child = parent.get_child("isr-enable", ISR_NS)?;
        let mechanism = child.attr("mechanism")?.to_string();
        Some(Self { mechanism })
    }
}

/// Build the `<isr-enabled token='...'/>` element to append inside
/// `<enabled/>` (XEP-0397 "Obtaining a Instant Stream Resumption Token").
pub fn isr_enabled_element(token: &str) -> Element {
    Element::builder("isr-enabled", ISR_NS)
        .attr(minidom::rxml::xml_ncname!("token").to_owned(), token)
        .build()
}

/// Build the `<isr xmlns='{ISR_NS}'><mechanisms .../></isr>` stream-feature
/// element (XEP-0397 "Stream Feature"). Only advertised by the caller when
/// `clustering.enabled && Postgres` (ADR-0017 Phase 3 Slice 8, Q8).
pub fn isr_stream_feature_element() -> Element {
    let mechanism = Element::builder("mechanism", crate::ns::SASL)
        .append(super::ISR_PINNED_MECHANISM)
        .build();
    let mechanisms = Element::builder("mechanisms", crate::ns::SASL)
        .append(mechanism)
        .build();
    Element::builder("isr", ISR_NS).append(mechanisms).build()
}

/// `<inst-resume with-isr-token='true'><resume .../></inst-resume>`, parsed
/// out of an inline child of a SASL2 `<authenticate/>` (XEP-0397
/// "Performing Instant Stream Resumption").
#[derive(Debug, Clone)]
pub struct InstResume {
    /// The only defined attribute of `<inst-resume/>`; defaults to `true`
    /// when omitted, per the XEP. This implementation only supports the
    /// `true` case (deviation: performing ISR resumption with a real,
    /// non-token SASL credential — `with-isr-token='false'` — rides general
    /// SASL2 authentication, which this codebase does not implement; see
    /// the phase plan).
    pub with_isr_token: bool,
    pub resume: SmResume,
}

impl InstResume {
    /// Look for an `<inst-resume/>` child (qualified by [`ISR_NS`]) inside
    /// `parent` (the `<authenticate/>` element) and parse it, if present.
    pub fn from_element(parent: &Element) -> Option<Self> {
        let child = parent.get_child("inst-resume", ISR_NS)?;
        let with_isr_token = child
            .attr("with-isr-token")
            .map(|value| matches!(value, "true" | "1"))
            .unwrap_or(true);
        let resume_element = child.get_child("resume", SM_NS)?;
        let resume = SmResume::from_element(resume_element)?;
        Some(Self {
            with_isr_token,
            resume,
        })
    }
}

/// Build `<inst-resumed token='...'><resumed .../></inst-resumed>` to nest
/// inside a SASL2 `<success/>` (XEP-0397 "Successful Stream Resumption").
pub fn inst_resumed_element(new_token: &str, resumed: &SmResumed) -> Element {
    Element::builder("inst-resumed", ISR_NS)
        .attr(minidom::rxml::xml_ncname!("token").to_owned(), new_token)
        .append(resumed.to_element())
        .build()
}

/// Build `<inst-resume-failed><failed .../></inst-resume-failed>` to nest
/// inside a SASL2 `<success/>` (XEP-0397 "Successful Authentication but
/// failed Stream Resumption").
pub fn inst_resume_failed_element(failed: &SmFailed) -> Element {
    Element::builder("inst-resume-failed", ISR_NS)
        .append(failed.to_element())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn isr_enable_parses_from_enable_element() {
        let xml = format!(
            "<enable xmlns='{SM_NS}'><isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/></enable>"
        );
        let element = Element::from_str(&xml).expect("valid xml");
        let isr_enable = IsrEnable::from_element(&element).expect("isr-enable present");
        assert_eq!(isr_enable.mechanism, "PLAIN");
    }

    #[test]
    fn isr_enable_absent_when_no_child() {
        let xml = format!("<enable xmlns='{SM_NS}'/>");
        let element = Element::from_str(&xml).expect("valid xml");
        assert!(IsrEnable::from_element(&element).is_none());
    }

    #[test]
    fn isr_enabled_element_has_correct_shape() {
        let element = isr_enabled_element("tok-123");
        assert_eq!(element.name(), "isr-enabled");
        assert_eq!(element.ns(), ISR_NS);
        assert_eq!(element.attr("token"), Some("tok-123"));
    }

    #[test]
    fn isr_stream_feature_lists_the_pinned_mechanism() {
        let element = isr_stream_feature_element();
        assert_eq!(element.name(), "isr");
        assert_eq!(element.ns(), ISR_NS);
        let mechanisms = element
            .get_child("mechanisms", crate::ns::SASL)
            .expect("mechanisms child");
        let mechanism = mechanisms
            .get_child("mechanism", crate::ns::SASL)
            .expect("mechanism child");
        assert_eq!(mechanism.text(), super::super::ISR_PINNED_MECHANISM);
    }

    #[test]
    fn inst_resume_parses_from_authenticate_element() {
        let xml = format!(
            "<authenticate xmlns='urn:xmpp:sasl:2' mechanism='PLAIN'>\
                <inst-resume xmlns='{ISR_NS}' with-isr-token='true'>\
                    <resume xmlns='{SM_NS}' h='5' previd='sm-1'/>\
                </inst-resume>\
             </authenticate>"
        );
        let element = Element::from_str(&xml).expect("valid xml");
        let inst_resume = InstResume::from_element(&element).expect("inst-resume present");
        assert!(inst_resume.with_isr_token);
        assert_eq!(inst_resume.resume.previd, "sm-1");
        assert_eq!(inst_resume.resume.h, 5);
    }

    #[test]
    fn inst_resume_defaults_with_isr_token_to_true() {
        let xml = format!(
            "<authenticate xmlns='urn:xmpp:sasl:2' mechanism='PLAIN'>\
                <inst-resume xmlns='{ISR_NS}'>\
                    <resume xmlns='{SM_NS}' h='0' previd='sm-1'/>\
                </inst-resume>\
             </authenticate>"
        );
        let element = Element::from_str(&xml).expect("valid xml");
        let inst_resume = InstResume::from_element(&element).expect("inst-resume present");
        assert!(inst_resume.with_isr_token);
    }

    #[test]
    fn inst_resumed_element_nests_resumed() {
        let resumed = SmResumed::new("sm-1".to_string(), 42);
        let element = inst_resumed_element("new-token", &resumed);
        assert_eq!(element.name(), "inst-resumed");
        assert_eq!(element.ns(), ISR_NS);
        assert_eq!(element.attr("token"), Some("new-token"));
        let nested = element.get_child("resumed", SM_NS).expect("resumed child");
        assert_eq!(nested.attr("previd"), Some("sm-1"));
        assert_eq!(nested.attr("h"), Some("42"));
    }

    #[test]
    fn inst_resume_failed_element_nests_failed() {
        let failed = SmFailed::resume_failed("resource-constraint", 7);
        let element = inst_resume_failed_element(&failed);
        assert_eq!(element.name(), "inst-resume-failed");
        assert_eq!(element.ns(), ISR_NS);
        let nested = element.get_child("failed", SM_NS).expect("failed child");
        assert_eq!(nested.attr("h"), Some("7"));
        assert!(nested
            .get_child("resource-constraint", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());
    }
}
