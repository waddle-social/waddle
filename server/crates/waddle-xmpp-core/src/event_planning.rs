//! Typed, independently persisted event-room planning objects.
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use jid::BareJid;
use minidom::Element;
use thiserror::Error;
use url::Url;

/// Room-scoped XEP-0060 node and Waddle payload namespace.
pub const NODE_EVENT_PLANNING: &str = "urn:waddle:event-planning:0";

/// Independently addressable planning object type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningKind {
    /// Overview, reserved identifier `event`.
    Event,
    /// Saved HTTP(S) link.
    Link,
    /// Assigned action.
    Task,
    /// Scheduled or proposed activity.
    Itinerary,
    /// Recorded agreement.
    Decision,
    /// Arrival or departure details.
    Travel,
    /// Shared cost in minor currency units.
    Expense,
    /// Question and named choices.
    Poll,
    /// One person's poll response.
    Vote,
    /// One person's attendance response.
    Attendance,
}
impl PlanningKind {
    /// Stable payload token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Link => "link",
            Self::Task => "task",
            Self::Itinerary => "itinerary",
            Self::Decision => "decision",
            Self::Travel => "travel",
            Self::Expense => "expense",
            Self::Poll => "poll",
            Self::Vote => "vote",
            Self::Attendance => "attendance",
        }
    }
    /// Parse a payload token.
    #[must_use]
    pub fn from_token(value: &str) -> Option<Self> {
        match value {
            "event" => Some(Self::Event),
            "link" => Some(Self::Link),
            "task" => Some(Self::Task),
            "itinerary" => Some(Self::Itinerary),
            "decision" => Some(Self::Decision),
            "travel" => Some(Self::Travel),
            "expense" => Some(Self::Expense),
            "poll" => Some(Self::Poll),
            "vote" => Some(Self::Vote),
            "attendance" => Some(Self::Attendance),
            _ => None,
        }
    }
}

/// An event member's attendance response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttendanceStatus {
    /// Awaiting response.
    Invited,
    /// Interested but undecided.
    Interested,
    /// Plans to attend.
    Going,
    /// Will not attend.
    Declined,
}
impl AttendanceStatus {
    /// Stable payload token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invited => "invited",
            Self::Interested => "interested",
            Self::Going => "going",
            Self::Declined => "declined",
        }
    }
    /// Parse a payload token.
    #[must_use]
    pub fn from_token(value: &str) -> Option<Self> {
        match value {
            "invited" => Some(Self::Invited),
            "interested" => Some(Self::Interested),
            "going" => Some(Self::Going),
            "declined" => Some(Self::Declined),
            _ => None,
        }
    }
}

/// Whether a proposed event or activity has been settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningStatus {
    /// Still under discussion.
    Proposed,
    /// Agreed upon.
    Confirmed,
    /// No longer happening.
    Cancelled,
}
impl PlanningStatus {
    /// Stable payload token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Confirmed => "confirmed",
            Self::Cancelled => "cancelled",
        }
    }
    /// Parse a payload token.
    #[must_use]
    pub fn from_token(value: &str) -> Option<Self> {
        match value {
            "proposed" => Some(Self::Proposed),
            "confirmed" => Some(Self::Confirmed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A stable poll choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningOption {
    /// Identifier within the poll.
    pub id: String,
    /// Human-readable choice.
    pub label: String,
}

/// One item, independently stored to avoid overwriting unrelated planning work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningItem {
    /// PubSub item identifier.
    pub id: String,
    /// Object type.
    pub kind: PlanningKind,
    /// Server-stamped original publisher; organizer edits preserve attribution.
    pub author: Option<BareJid>,
    /// Human-readable name; empty for votes and attendance.
    pub title: String,
    /// Additional notes.
    pub description: Option<String>,
    /// HTTP(S) resource without credentials.
    pub url: Option<Url>,
    /// Grouping such as Accommodation.
    pub category: Option<String>,
    /// Place or address.
    pub location: Option<String>,
    /// IANA timezone for event and itinerary display.
    pub timezone: Option<Tz>,
    /// Start, arrival or task deadline.
    pub starts_at: Option<DateTime<Utc>>,
    /// End or departure.
    pub ends_at: Option<DateTime<Utc>>,
    /// Responsible person.
    pub assignee: Option<BareJid>,
    /// Task completion.
    pub completed: bool,
    /// Highlight on the overview.
    pub pinned: bool,
    /// Non-negative cost in integer minor units.
    pub amount_minor: Option<i64>,
    /// Uppercase three-letter currency.
    pub currency: Option<String>,
    /// People sharing the expense.
    pub participants: Vec<BareJid>,
    /// Poll choices.
    pub options: Vec<PlanningOption>,
    /// Chosen poll option identifiers.
    pub selections: Vec<String>,
    /// Referenced poll identifier.
    pub subject: Option<String>,
    /// Member attendance status.
    pub attendance: Option<AttendanceStatus>,
    /// Event or activity status.
    pub status: Option<PlanningStatus>,
}

/// Invalid planning payload or inconsistent values.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct PlanningError(pub &'static str);

impl PlanningItem {
    /// Construct an object with optional fields empty.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: PlanningKind, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            author: None,
            title: title.into(),
            description: None,
            url: None,
            category: None,
            location: None,
            timezone: None,
            starts_at: None,
            ends_at: None,
            assignee: None,
            completed: false,
            pinned: false,
            amount_minor: None,
            currency: None,
            participants: Vec::new(),
            options: Vec::new(),
            selections: Vec::new(),
            subject: None,
            attendance: None,
            status: None,
        }
    }
    /// Canonical identifier binding a vote or RSVP to its authenticated publisher.
    #[must_use]
    pub fn self_owned_id(&self, publisher: &BareJid) -> Option<String> {
        match self.kind {
            PlanningKind::Attendance => Some(format!("attendance:{publisher}")),
            PlanningKind::Vote => self
                .subject
                .as_ref()
                .map(|poll| format!("vote:{poll}:{publisher}")),
            _ => None,
        }
    }
    /// Validate field bounds and kind-specific requirements.
    ///
    /// # Errors
    /// Returns the first invalid field or inconsistent combination.
    pub fn validate(&self) -> Result<(), PlanningError> {
        if self.id.is_empty() || self.id.len() > 512 || self.id.chars().any(char::is_control) {
            return Err(PlanningError("invalid item identifier"));
        }
        if (self.kind == PlanningKind::Event) != (self.id == "event") {
            return Err(PlanningError(
                "event metadata must use the reserved event identifier",
            ));
        }
        if !matches!(self.kind, PlanningKind::Attendance | PlanningKind::Vote)
            && (self.id.len() > 128 || self.id.contains(':'))
        {
            return Err(PlanningError("invalid object identifier"));
        }
        if !matches!(self.kind, PlanningKind::Vote | PlanningKind::Attendance)
            && self.title.trim().is_empty()
        {
            return Err(PlanningError("a title is required"));
        }
        if self.title.len() > 512
            || self.description.as_ref().is_some_and(|v| v.len() > 16_384)
            || self.category.as_ref().is_some_and(|v| v.len() > 128)
            || self.location.as_ref().is_some_and(|v| v.len() > 2048)
        {
            return Err(PlanningError("planning text is too long"));
        }
        if self.url.as_ref().is_some_and(|url| {
            !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.as_str().len() > 8192
        }) {
            return Err(PlanningError(
                "links require HTTP(S) URLs without credentials",
            ));
        }
        if self
            .starts_at
            .zip(self.ends_at)
            .is_some_and(|(start, end)| end <= start)
            || (self.ends_at.is_some() && self.starts_at.is_none())
        {
            return Err(PlanningError("end must follow a supplied start"));
        }
        if self.kind == PlanningKind::Link && self.url.is_none() {
            return Err(PlanningError("a saved link requires a URL"));
        }
        if self
            .amount_minor
            .is_some_and(|value| !(0..=9_007_199_254_740_991).contains(&value))
        {
            return Err(PlanningError("amount must be a safe non-negative integer"));
        }
        if self.kind == PlanningKind::Expense
            && (self.amount_minor.is_none()
                || self
                    .currency
                    .as_ref()
                    .is_none_or(|v| v.len() != 3 || !v.bytes().all(|b| b.is_ascii_uppercase())))
        {
            return Err(PlanningError(
                "an expense requires a safe non-negative integer amount and three-letter currency",
            ));
        }
        if self.participants.len() > 100 || self.options.len() > 50 || self.selections.len() > 50 {
            return Err(PlanningError("too many participants or choices"));
        }
        if self.kind == PlanningKind::Poll {
            if self.options.len() < 2
                || self.options.iter().any(|v| {
                    v.id.is_empty()
                        || v.id.len() > 128
                        || v.label.trim().is_empty()
                        || v.label.len() > 512
                })
            {
                return Err(PlanningError("a poll requires at least two named choices"));
            }
            let unique: std::collections::HashSet<_> = self.options.iter().map(|v| &v.id).collect();
            if unique.len() != self.options.len() {
                return Err(PlanningError("poll choice identifiers must be unique"));
            }
        }
        if self.kind == PlanningKind::Vote
            && (self
                .subject
                .as_ref()
                .is_none_or(|v| v.is_empty() || v.len() > 128 || v.contains(':'))
                || self.selections.is_empty()
                || self
                    .selections
                    .iter()
                    .any(|v| v.is_empty() || v.len() > 128))
        {
            return Err(PlanningError("a vote requires a poll and a selection"));
        }
        if self.kind == PlanningKind::Attendance && self.attendance.is_none() {
            return Err(PlanningError("an attendance response requires a status"));
        }
        Ok(())
    }

    /// Serialize this object to its namespaced XML payload.
    #[must_use]
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("item", NODE_EVENT_PLANNING)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), &self.id)
            .attr(
                minidom::rxml::xml_ncname!("kind").to_owned(),
                self.kind.as_str(),
            );
        if let Some(author) = &self.author {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("author").to_owned(),
                author.as_str(),
            );
        }
        let mut element = builder.build();
        let mut add = |name: &str, text: &str| {
            element.append_child(
                Element::builder(name, NODE_EVENT_PLANNING)
                    .append(text)
                    .build(),
            );
        };
        add("title", &self.title);
        for (name, value) in [
            ("description", &self.description),
            ("category", &self.category),
            ("location", &self.location),
            ("currency", &self.currency),
            ("subject", &self.subject),
        ] {
            if let Some(value) = value {
                add(name, value);
            }
        }
        if let Some(url) = &self.url {
            add("url", url.as_str());
        }
        if let Some(zone) = &self.timezone {
            add("timezone", zone.name());
        }
        if let Some(start) = self.starts_at {
            add("starts-at", &start.to_rfc3339());
        }
        if let Some(end) = self.ends_at {
            add("ends-at", &end.to_rfc3339());
        }
        if let Some(assignee) = &self.assignee {
            add("assignee", assignee.as_str());
        }
        if self.completed {
            add("completed", "true");
        }
        if self.pinned {
            add("pinned", "true");
        }
        if let Some(amount) = self.amount_minor {
            add("amount-minor", &amount.to_string());
        }
        if let Some(status) = self.status {
            add("status", status.as_str());
        }
        if let Some(attendance) = self.attendance {
            add("attendance", attendance.as_str());
        }
        for participant in &self.participants {
            add("participant", participant.as_str());
        }
        for selection in &self.selections {
            add("selection", selection);
        }
        for option in &self.options {
            element.append_child(
                Element::builder("option", NODE_EVENT_PLANNING)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), &option.id)
                    .append(option.label.as_str())
                    .build(),
            );
        }
        element
    }

    /// Parse a bounded, typed object from its XML payload.
    ///
    /// # Errors
    /// Returns a validation error for malformed values or ambiguous fields.
    pub fn from_element(element: &Element) -> Result<Self, PlanningError> {
        if !element.is("item", NODE_EVENT_PLANNING) {
            return Err(PlanningError("invalid planning payload"));
        }
        let kind = element
            .attr("kind")
            .and_then(PlanningKind::from_token)
            .ok_or(PlanningError("invalid planning kind"))?;
        let mut item = Self::new(
            element
                .attr("id")
                .ok_or(PlanningError("missing item identifier"))?,
            kind,
            "",
        );
        item.author = element
            .attr("author")
            .map(str::parse)
            .transpose()
            .map_err(|_| PlanningError("invalid author JID"))?;
        let mut scalar_names = std::collections::HashSet::new();
        for child in element.children() {
            if child.ns() != NODE_EVENT_PLANNING {
                return Err(PlanningError("unexpected planning namespace"));
            }
            let text = child.text();
            if !matches!(child.name(), "participant" | "selection" | "option")
                && !scalar_names.insert(child.name())
            {
                return Err(PlanningError("duplicate planning field"));
            }
            match child.name() {
                "title" => item.title = text,
                "description" => item.description = Some(text),
                "category" => item.category = Some(text),
                "location" => item.location = Some(text),
                "currency" => item.currency = Some(text),
                "subject" => item.subject = Some(text),
                "url" => {
                    item.url = Some(Url::parse(&text).map_err(|_| PlanningError("invalid URL"))?)
                }
                "timezone" => {
                    item.timezone = Some(
                        text.parse()
                            .map_err(|_| PlanningError("invalid IANA timezone"))?,
                    )
                }
                "starts-at" => item.starts_at = Some(parse_instant(&text)?),
                "ends-at" => item.ends_at = Some(parse_instant(&text)?),
                "assignee" => {
                    item.assignee = Some(
                        text.parse()
                            .map_err(|_| PlanningError("invalid assignee JID"))?,
                    )
                }
                "completed" => item.completed = parse_bool(&text)?,
                "pinned" => item.pinned = parse_bool(&text)?,
                "amount-minor" => {
                    item.amount_minor = Some(
                        text.parse()
                            .map_err(|_| PlanningError("invalid expense amount"))?,
                    )
                }
                "status" => {
                    item.status = Some(
                        PlanningStatus::from_token(&text)
                            .ok_or(PlanningError("invalid planning status"))?,
                    )
                }
                "attendance" => {
                    item.attendance = Some(
                        AttendanceStatus::from_token(&text)
                            .ok_or(PlanningError("invalid attendance status"))?,
                    )
                }
                "participant" => item.participants.push(
                    text.parse()
                        .map_err(|_| PlanningError("invalid participant JID"))?,
                ),
                "selection" => item.selections.push(text),
                "option" => item.options.push(PlanningOption {
                    id: child
                        .attr("id")
                        .ok_or(PlanningError("missing option identifier"))?
                        .to_string(),
                    label: text,
                }),
                _ => return Err(PlanningError("unknown planning field")),
            }
        }
        item.validate()?;
        Ok(item)
    }
}

fn parse_instant(value: &str) -> Result<DateTime<Utc>, PlanningError> {
    DateTime::parse_from_rfc3339(value)
        .map(|v| v.with_timezone(&Utc))
        .map_err(|_| PlanningError("invalid timestamp"))
}
fn parse_bool(value: &str) -> Result<bool, PlanningError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(PlanningError("invalid boolean")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planning_round_trip_preserves_trip_details_and_xml_escaping() {
        let mut item = PlanningItem::new(
            "dinner",
            PlanningKind::Itinerary,
            "Dinner & discussion <London>",
        );
        item.author = Some("alice@example.com".parse().expect("JID"));
        item.description = Some("Meet outside; bring ideas & questions".into());
        item.location = Some("King's Cross".into());
        item.timezone = Some("Europe/London".parse().expect("timezone"));
        item.starts_at = Some(parse_instant("2027-02-12T19:00:00Z").expect("start"));
        item.ends_at = Some(parse_instant("2027-02-12T21:00:00Z").expect("end"));
        item.assignee = item.author.clone();
        item.pinned = true;
        item.status = Some(PlanningStatus::Confirmed);
        let wire = String::from(&item.to_element());
        assert!(wire.contains("&amp;"));
        let parsed: Element = wire.parse().expect("XML");
        assert_eq!(PlanningItem::from_element(&parsed).expect("planning"), item);
    }

    #[test]
    fn planning_unscheduled_overview_and_all_object_types_round_trip() {
        for kind in [
            PlanningKind::Event,
            PlanningKind::Link,
            PlanningKind::Task,
            PlanningKind::Itinerary,
            PlanningKind::Decision,
            PlanningKind::Travel,
            PlanningKind::Expense,
            PlanningKind::Poll,
            PlanningKind::Vote,
            PlanningKind::Attendance,
        ] {
            let mut item = PlanningItem::new(
                if kind == PlanningKind::Event {
                    "event"
                } else {
                    "object"
                },
                kind,
                "London maintainers meetup",
            );
            item.url = Some(Url::parse("https://example.com/?a=1&b=2").expect("URL"));
            item.amount_minor = Some(12_345);
            item.currency = Some("GBP".into());
            item.participants = vec!["alice@example.com".parse().expect("JID")];
            item.options = vec![
                PlanningOption {
                    id: "a".into(),
                    label: "Friday".into(),
                },
                PlanningOption {
                    id: "b".into(),
                    label: "Saturday".into(),
                },
            ];
            item.subject = Some("poll".into());
            item.selections = vec!["a".into()];
            item.attendance = Some(AttendanceStatus::Interested);
            item.validate().expect("valid item");
            assert_eq!(
                PlanningItem::from_element(&item.to_element()).expect("round trip"),
                item
            );
            assert_eq!(item.starts_at, None);
        }
    }

    #[test]
    fn planning_rejects_unsafe_links_and_inconsistent_dates() {
        let mut item = PlanningItem::new("link", PlanningKind::Link, "Hotel");
        for value in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user:password@example.com/",
        ] {
            item.url = Some(Url::parse(value).expect("URL"));
            assert!(item.validate().is_err(), "{value}");
        }
        item.url = Some(Url::parse("https://example.com/").expect("URL"));
        item.ends_at = Some(parse_instant("2027-02-12T21:00:00Z").expect("end"));
        assert!(item.validate().is_err());
        item.starts_at = item.ends_at;
        assert!(item.validate().is_err());
        item.starts_at = Some(parse_instant("2027-02-12T19:00:00Z").expect("start"));
        assert!(item.validate().is_ok());
    }

    #[test]
    fn planning_rejects_ambiguous_scalars_and_malformed_values() {
        for (name, value) in [
            ("title", "duplicate"),
            ("timezone", "Mars/Olympus"),
            ("starts-at", "next Friday"),
            ("assignee", "person@example.com/resource"),
            ("pinned", "yes"),
            ("unknown", "surprise"),
        ] {
            let mut payload = PlanningItem::new("task", PlanningKind::Task, "Task").to_element();
            payload.append_child(
                Element::builder(name, NODE_EVENT_PLANNING)
                    .append(value)
                    .build(),
            );
            assert!(PlanningItem::from_element(&payload).is_err(), "{name}");
        }
    }

    #[test]
    fn planning_poll_ids_are_unique_and_responses_are_bound_to_member() {
        let publisher: BareJid = "alice@example.com".parse().expect("JID");
        let mut poll = PlanningItem::new("dinner", PlanningKind::Poll, "Where shall we eat?");
        poll.options = vec![
            PlanningOption {
                id: "a".into(),
                label: "Pub".into(),
            },
            PlanningOption {
                id: "a".into(),
                label: "Restaurant".into(),
            },
        ];
        assert!(poll.validate().is_err());
        poll.options[1].id = "b".into();
        assert!(poll.validate().is_ok());
        let mut vote = PlanningItem::new("vote:dinner:alice@example.com", PlanningKind::Vote, "");
        vote.subject = Some("dinner".into());
        vote.selections = vec!["a".into()];
        assert_eq!(
            vote.self_owned_id(&publisher).as_deref(),
            Some("vote:dinner:alice@example.com")
        );
        let mut response =
            PlanningItem::new("attendance:alice@example.com", PlanningKind::Attendance, "");
        response.attendance = Some(AttendanceStatus::Going);
        assert_eq!(
            response.self_owned_id(&publisher).as_deref(),
            Some("attendance:alice@example.com")
        );
        assert!(response.validate().is_ok());
        response.id = "event".into();
        assert!(response.validate().is_err());
    }

    #[test]
    fn planning_expenses_use_nonnegative_integer_minor_units() {
        let mut expense = PlanningItem::new("hotel", PlanningKind::Expense, "Hotel deposit");
        expense.amount_minor = Some(-1);
        expense.currency = Some("GBP".into());
        assert!(expense.validate().is_err());
        expense.amount_minor = Some(20_000);
        expense.currency = Some("gbp".into());
        assert!(expense.validate().is_err());
        expense.currency = Some("GBP".into());
        assert!(expense.validate().is_ok());
        expense.amount_minor = Some(9_007_199_254_740_992);
        assert!(
            expense.validate().is_err(),
            "every persisted amount must round-trip through JavaScript"
        );
        expense.kind = PlanningKind::Task;
        assert!(
            expense.validate().is_err(),
            "optional amounts on every object must be web-readable"
        );
    }
}
