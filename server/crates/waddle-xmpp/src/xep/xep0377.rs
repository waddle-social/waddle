//! XEP-0377: Blocking Command Reports
//!
//! Extends XEP-0191 (Blocking Command) with an optional `<report/>` inside a
//! blocking `<item/>`. The server blocks the JID exactly as it would without the
//! report (§5) and may then act on the report. Waddle records each report for
//! its operators through a structured log line and a counter; it does not
//! forward reports to other domains or third parties, so the `<report-origin/>`
//! and `<third-party/>` opt-ins are parsed but never acted on (§4: servers MAY
//! ignore processing options).
//!
//! ## XML Format
//!
//! Block with spam report:
//! ```xml
//! <iq type='set' id='block-report-1'>
//!   <block xmlns='urn:xmpp:blocking'>
//!     <item jid='spammer@example.com'>
//!       <report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'/>
//!     </item>
//!   </block>
//! </iq>
//! ```
//!
//! ## Report Reasons
//!
//! - `urn:xmpp:reporting:spam`: a JID sending unwanted messages
//! - `urn:xmpp:reporting:abuse`: general abuse
//!
//! Other reason URIs may be registered by other specifications. A report
//! with an unregistered reason is still recorded and the JID is still blocked.

use jid::{BareJid, Jid};
use minidom::Element;
use waddle_xmpp_core::xep0359::{is_stanza_id_element, StanzaId};

/// Namespace for XEP-0377 Blocking Command Reports.
pub const NS_REPORTING: &str = "urn:xmpp:reporting:1";

const REASON_SPAM: &str = "urn:xmpp:reporting:spam";
const REASON_ABUSE: &str = "urn:xmpp:reporting:abuse";
const MAX_REASON_LEN: usize = 512;
/// Report text longer than this many characters is truncated in the
/// operator log; the full text stays in the parsed [`Report`].
const LOG_TEXT_LIMIT: usize = 1024;
/// At most this many reported stanza-ids are listed in the operator log.
const LOG_STANZA_ID_LIMIT: usize = 16;

/// A reason URI that is not one of the reasons defined by XEP-0377.
///
/// Reasons are registered by separate specifications (§ Abuse Reporting
/// Registry), so a server can legitimately receive one it does not know.
/// The value is a validated absolute URI: non-empty, bounded, no whitespace
/// or control characters, and a scheme separator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReasonUri(String);

impl ReasonUri {
    /// Validate `value` as a reason URI.
    pub fn parse(value: &str) -> Result<Self, ReportError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_REASON_LEN
            && !value.chars().any(|c| c.is_whitespace() || c.is_control())
            && value
                .split_once(':')
                .is_some_and(|(scheme, rest)| !scheme.is_empty() && !rest.is_empty());
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(ReportError::MalformedReason)
        }
    }

    /// The URI as written on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The reason for a report (`reason` attribute of `<report/>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReportReason {
    /// `urn:xmpp:reporting:spam`: a JID that is sending unwanted messages.
    Spam,
    /// `urn:xmpp:reporting:abuse`: general abuse.
    Abuse,
    /// A well-formed reason URI registered elsewhere.
    Other(ReasonUri),
}

impl ReportReason {
    /// Parse the `reason` attribute value.
    pub fn parse(value: &str) -> Result<Self, ReportError> {
        match value {
            REASON_SPAM => Ok(Self::Spam),
            REASON_ABUSE => Ok(Self::Abuse),
            other => ReasonUri::parse(other).map(Self::Other),
        }
    }

    /// The reason URI as written on the wire.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Spam => REASON_SPAM,
            Self::Abuse => REASON_ABUSE,
            Self::Other(uri) => uri.as_str(),
        }
    }
}

impl std::fmt::Display for ReportReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A user-provided `<text/>` child, with its optional `xml:lang`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportText {
    /// Natural language of the text (`xml:lang`), when given.
    pub lang: Option<String>,
    /// The text.
    pub text: String,
}

/// Why a `<report/>` element was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    /// The element is not `<report xmlns='urn:xmpp:reporting:1'/>`.
    #[error("not a urn:xmpp:reporting:1 report element")]
    NotReport,
    /// §3: "Abuse reports MUST include a reason for the report".
    #[error("report has no reason attribute")]
    MissingReason,
    /// The `reason` attribute is not a well-formed URI.
    #[error("report reason is not a well-formed URI")]
    MalformedReason,
}

/// A report attached to one blocked JID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The reason for the report.
    pub reason: ReportReason,
    /// User-provided explanations (§3), one per language.
    pub texts: Vec<ReportText>,
    /// XEP-0359 stanza-ids of the reported messages (§5). Entries without a
    /// valid `by` JID and `id` are dropped.
    pub stanza_ids: Vec<StanzaId>,
    /// §4: the user approved forwarding to the originating domain.
    pub report_origin: bool,
    /// §4: the user approved forwarding to third-party processors.
    pub third_party: bool,
}

impl Report {
    fn with_reason(reason: ReportReason) -> Self {
        Self {
            reason,
            texts: Vec::new(),
            stanza_ids: Vec::new(),
            report_origin: false,
            third_party: false,
        }
    }

    /// Create a spam report.
    pub fn spam() -> Self {
        Self::with_reason(ReportReason::Spam)
    }

    /// Create an abuse report.
    pub fn abuse() -> Self {
        Self::with_reason(ReportReason::Abuse)
    }

    /// Add explanatory text without a language tag.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.texts.push(ReportText {
            lang: None,
            text: text.into(),
        });
        self
    }

    /// Add explanatory text in `lang`.
    pub fn with_text_lang(mut self, lang: impl Into<String>, text: impl Into<String>) -> Self {
        self.texts.push(ReportText {
            lang: Some(lang.into()),
            text: text.into(),
        });
        self
    }

    /// Reference a reported message by stanza-id.
    pub fn with_stanza_id(mut self, stanza_id: StanzaId) -> Self {
        self.stanza_ids.push(stanza_id);
        self
    }

    /// The first user-provided text, if any.
    pub fn first_text(&self) -> Option<&str> {
        self.texts.first().map(|t| t.text.as_str())
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<report/>` element.
pub fn is_report_element(elem: &Element) -> bool {
    elem.ns() == NS_REPORTING && elem.name() == "report"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a report from a `<report/>` element.
///
/// A missing `reason` is an error (§3 MUST). Unknown children are ignored so
/// future extensions do not break blocking.
pub fn parse_report(elem: &Element) -> Result<Report, ReportError> {
    if !is_report_element(elem) {
        return Err(ReportError::NotReport);
    }
    let reason = ReportReason::parse(elem.attr("reason").ok_or(ReportError::MissingReason)?)?;
    let mut report = Report::with_reason(reason);

    for child in elem.children() {
        if child.ns() == NS_REPORTING {
            match child.name() {
                "text" => {
                    let text = child.text();
                    if !text.is_empty() {
                        report.texts.push(ReportText {
                            lang: child
                                .attr_ns(&minidom::rxml::Namespace::XML, "lang")
                                .map(str::to_owned),
                            text,
                        });
                    }
                }
                "report-origin" => report.report_origin = true,
                "third-party" => report.third_party = true,
                _ => {}
            }
        } else if is_stanza_id_element(child) {
            let id = child.attr("id").filter(|s| !s.is_empty());
            let by = child.attr("by").and_then(|by| by.parse::<Jid>().ok());
            if let (Some(id), Some(by)) = (id, by) {
                report.stanza_ids.push(StanzaId::new(id, by));
            }
        }
    }

    Ok(report)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<report xmlns='urn:xmpp:reporting:1' reason='...'/>` element.
pub fn build_report_element(report: &Report) -> Element {
    let mut elem = Element::builder("report", NS_REPORTING)
        .attr(
            minidom::rxml::xml_ncname!("reason").to_owned(),
            report.reason.as_str(),
        )
        .build();

    for stanza_id in &report.stanza_ids {
        elem.append_child(waddle_xmpp_core::xep0359::build_stanza_id_element(
            &stanza_id.id,
            &stanza_id.by,
        ));
    }
    for text in &report.texts {
        let mut text_elem = Element::builder("text", NS_REPORTING);
        if let Some(lang) = &text.lang {
            text_elem = text_elem.attr_ns(
                minidom::rxml::Namespace::XML,
                minidom::rxml::xml_ncname!("lang").to_owned(),
                lang.as_str(),
            );
        }
        let mut text_elem = text_elem.build();
        text_elem.append_text_node(text.text.as_str());
        elem.append_child(text_elem);
    }
    if report.report_origin {
        elem.append_child(Element::builder("report-origin", NS_REPORTING).build());
    }
    if report.third_party {
        elem.append_child(Element::builder("third-party", NS_REPORTING).build());
    }

    elem
}

// ── Recording ────────────────────────────────────────────────────────

impl From<&ReportReason> for crate::telemetry::attributes::AbuseReportReason {
    fn from(reason: &ReportReason) -> Self {
        match reason {
            ReportReason::Spam => Self::Spam,
            ReportReason::Abuse => Self::Abuse,
            ReportReason::Other(_) => Self::Other,
        }
    }
}

/// Record an accepted report for the operator: one structured
/// `info` event on target `waddle::abuse_report` and one
/// `waddle.abuse_reports.received` increment.
///
/// Call only after the block itself succeeded. Nothing is forwarded: the
/// §4 processing opt-ins are logged as given but not acted on.
pub fn record_report(reporter: &BareJid, reported: &Jid, report: &Report) {
    use crate::telemetry::attributes::AbuseReportReason;

    let log_text = report
        .first_text()
        .map(|text| text.chars().take(LOG_TEXT_LIMIT).collect::<String>());
    let stanza_ids: Vec<String> = report
        .stanza_ids
        .iter()
        .take(LOG_STANZA_ID_LIMIT)
        .map(|sid| format!("{}@{}", sid.id, sid.by))
        .collect();
    tracing::info!(
        target: "waddle::abuse_report",
        reporter = %reporter,
        reported = %reported,
        reason = %report.reason,
        text = log_text.as_deref(),
        stanza_id_count = report.stanza_ids.len(),
        stanza_ids = ?stanza_ids,
        report_origin_opt_in = report.report_origin,
        third_party_opt_in = report.third_party,
        "abuse report received with block"
    );
    crate::counter_add!(
        "waddle.abuse_reports.received",
        "{report}",
        "XEP-0377 reports received attached to a block, by reason",
        1,
        AbuseReportReason::from(&report.reason),
    );
}
