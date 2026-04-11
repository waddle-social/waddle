//! XEP-0377: Spam Reporting
//!
//! Extends XEP-0191 (Blocking Command) with abuse/spam reporting.
//! When blocking a user, the client can include a reason why, enabling
//! the server to take action on reported accounts.
//!
//! ## XML Format
//!
//! Block with spam report:
//! ```xml
//! <iq type='set' id='block-report-1'>
//!   <block xmlns='urn:xmpp:blocking'>
//!     <item jid='spammer@example.com'>
//!       <report xmlns='urn:xmpp:reporting:1' reason='spam'/>
//!     </item>
//!   </block>
//! </iq>
//! ```
//!
//! ## Report Reasons
//!
//! - **spam**: Unsolicited commercial messages
//! - **abuse**: Harassment, threats, hate speech
//!
//! ## Use Cases
//!
//! - Report + block spammers in one action
//! - Server-side abuse tracking and automated action
//! - Community moderation analytics

use minidom::Element;

/// Namespace for XEP-0377 Spam Reporting.
pub const NS_REPORTING: &str = "urn:xmpp:reporting:1";

/// Report reason categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportReason {
    /// Unsolicited commercial messages.
    Spam,
    /// Harassment, threats, or hate speech.
    Abuse,
}

impl ReportReason {
    /// Parse from attribute string.
    pub fn from_str_attr(s: &str) -> Option<Self> {
        match s {
            "spam" => Some(Self::Spam),
            "abuse" => Some(Self::Abuse),
            _ => None,
        }
    }

    /// Convert to attribute string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Abuse => "abuse",
        }
    }
}

impl std::fmt::Display for ReportReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A user report attached to a blocking action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The reason for the report.
    pub reason: ReportReason,
    /// Optional free-text explanation.
    pub text: Option<String>,
}

impl Report {
    /// Create a spam report.
    pub fn spam() -> Self {
        Self {
            reason: ReportReason::Spam,
            text: None,
        }
    }

    /// Create an abuse report.
    pub fn abuse() -> Self {
        Self {
            reason: ReportReason::Abuse,
            text: None,
        }
    }

    /// Add explanatory text.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// A complete report record (for server-side storage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRecord {
    /// Who filed the report.
    pub reporter_jid: String,
    /// Who was reported.
    pub reported_jid: String,
    /// The report details.
    pub report: Report,
    /// Timestamp when filed.
    pub timestamp: Option<String>,
}

impl ReportRecord {
    /// Create a new report record.
    pub fn new(
        reporter_jid: impl Into<String>,
        reported_jid: impl Into<String>,
        report: Report,
    ) -> Self {
        Self {
            reporter_jid: reporter_jid.into(),
            reported_jid: reported_jid.into(),
            report,
            timestamp: None,
        }
    }

    /// Set the timestamp.
    pub fn with_timestamp(mut self, ts: impl Into<String>) -> Self {
        self.timestamp = Some(ts.into());
        self
    }
}

/// Report storage for server-side tracking.
#[derive(Debug, Default)]
pub struct ReportStore {
    reports: Vec<ReportRecord>,
}

impl ReportStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// File a report.
    pub fn file_report(&mut self, record: ReportRecord) {
        self.reports.push(record);
    }

    /// Get all reports for a specific JID.
    pub fn reports_for(&self, reported_jid: &str) -> Vec<&ReportRecord> {
        self.reports
            .iter()
            .filter(|r| r.reported_jid == reported_jid)
            .collect()
    }

    /// Count reports for a JID.
    pub fn report_count(&self, reported_jid: &str) -> usize {
        self.reports
            .iter()
            .filter(|r| r.reported_jid == reported_jid)
            .count()
    }

    /// Get all spam reports.
    pub fn spam_reports(&self) -> Vec<&ReportRecord> {
        self.reports
            .iter()
            .filter(|r| r.report.reason == ReportReason::Spam)
            .collect()
    }

    /// Total reports.
    pub fn total(&self) -> usize {
        self.reports.len()
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<report/>` element.
pub fn is_report_element(elem: &Element) -> bool {
    elem.ns() == NS_REPORTING && elem.name() == "report"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a report from a `<report/>` element.
pub fn parse_report(elem: &Element) -> Option<Report> {
    if !is_report_element(elem) {
        return None;
    }

    let reason = elem.attr("reason").and_then(ReportReason::from_str_attr)?;

    let text = elem
        .children()
        .find(|c| c.name() == "text")
        .map(|c| c.text())
        .filter(|t| !t.is_empty());

    Some(Report { reason, text })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<report xmlns='urn:xmpp:reporting:1' reason='...'/>` element.
pub fn build_report_element(report: &Report) -> Element {
    let mut elem = Element::builder("report", NS_REPORTING)
        .attr("reason", report.reason.as_str())
        .build();

    if let Some(ref text) = report.text {
        let mut text_elem = Element::builder("text", NS_REPORTING).build();
        text_elem.append_text_node(text);
        elem.append_child(text_elem);
    }

    elem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_report_reason_roundtrip() {
        for reason in [ReportReason::Spam, ReportReason::Abuse] {
            let s = reason.as_str();
            assert_eq!(ReportReason::from_str_attr(s), Some(reason));
        }
        assert_eq!(ReportReason::from_str_attr("unknown"), None);
    }

    #[test]
    fn test_report_reason_display() {
        assert_eq!(ReportReason::Spam.to_string(), "spam");
        assert_eq!(ReportReason::Abuse.to_string(), "abuse");
    }

    #[test]
    fn test_is_report_element() {
        let elem = Element::builder("report", NS_REPORTING)
            .attr("reason", "spam")
            .build();
        assert!(is_report_element(&elem));

        let wrong = Element::builder("report", "jabber:client").build();
        assert!(!is_report_element(&wrong));
    }

    #[test]
    fn test_build_and_parse_spam() {
        let report = Report::spam();
        let elem = build_report_element(&report);

        assert_eq!(elem.attr("reason"), Some("spam"));
        let parsed = parse_report(&elem).expect("parseable");
        assert_eq!(parsed.reason, ReportReason::Spam);
        assert_eq!(parsed.text, None);
    }

    #[test]
    fn test_build_and_parse_abuse_with_text() {
        let report = Report::abuse().with_text("Threatening messages");
        let elem = build_report_element(&report);

        let parsed = parse_report(&elem).expect("parseable");
        assert_eq!(parsed.reason, ReportReason::Abuse);
        assert_eq!(parsed.text.as_deref(), Some("Threatening messages"));
    }

    #[test]
    fn test_parse_wrong_element() {
        let elem = Element::builder("other", NS_REPORTING).build();
        assert!(parse_report(&elem).is_none());
    }

    #[test]
    fn test_parse_missing_reason() {
        let elem = Element::builder("report", NS_REPORTING).build();
        assert!(parse_report(&elem).is_none());
    }

    #[test]
    fn test_report_store() {
        let mut store = ReportStore::new();
        assert_eq!(store.total(), 0);

        store.file_report(ReportRecord::new(
            "alice@example.com",
            "spammer@example.com",
            Report::spam(),
        ));
        store.file_report(ReportRecord::new(
            "bob@example.com",
            "spammer@example.com",
            Report::spam().with_text("Sending ads"),
        ));
        store.file_report(ReportRecord::new(
            "alice@example.com",
            "troll@example.com",
            Report::abuse(),
        ));

        assert_eq!(store.total(), 3);
        assert_eq!(store.report_count("spammer@example.com"), 2);
        assert_eq!(store.report_count("troll@example.com"), 1);
        assert_eq!(store.report_count("innocent@example.com"), 0);
        assert_eq!(store.spam_reports().len(), 2);
        assert_eq!(store.reports_for("troll@example.com").len(), 1);
    }

    #[test]
    fn test_report_record_builder() {
        let record =
            ReportRecord::new("a@b", "c@d", Report::spam()).with_timestamp("2024-06-01T12:00:00Z");
        assert_eq!(record.reporter_jid, "a@b");
        assert_eq!(record.reported_jid, "c@d");
        assert_eq!(record.timestamp.as_deref(), Some("2024-06-01T12:00:00Z"));
    }

    #[test]
    fn test_namespace_constant() {
        assert_eq!(NS_REPORTING, "urn:xmpp:reporting:1");
    }

    #[test]
    fn test_report_store_empty() {
        let store = ReportStore::new();
        assert_eq!(store.total(), 0);
        assert!(store.spam_reports().is_empty());
        assert!(store.reports_for("anyone").is_empty());
    }

    #[test]
    fn test_report_constructors() {
        assert_eq!(Report::spam().reason, ReportReason::Spam);
        assert_eq!(Report::abuse().reason, ReportReason::Abuse);
    }
}
