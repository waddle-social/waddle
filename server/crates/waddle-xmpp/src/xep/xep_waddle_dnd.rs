//! Waddle Do-Not-Disturb wire shape (`urn:waddle:dnd:0`).
//!
//! DND is a user-explicit "suppress my push notifications" state with
//! two layered triggers: a one-shot snooze (`<snooze until='…'/>`) and
//! a weekly schedule (`<rule days='…' start='…' end='…'/>`). XMPP and
//! the XSF do not define a "do not disturb" payload — XEP-0319
//! covers idle detection only, not user-explicit DND — so the wire
//! lives in the project-local `urn:waddle:dnd:0` namespace per the
//! CLAUDE.md XEP-conformance hard rule.
//!
//! ## Wire shape
//!
//! Published to the owner's own PEP node `urn:waddle:dnd:0` (single
//! item, id `current` per XEP-0163 convention):
//!
//! ```xml
//! <dnd xmlns='urn:waddle:dnd:0' timezone='Europe/Oslo'>
//!   <snooze until='2026-05-23T17:00:00Z'/>
//!   <rule days='mon,tue,wed,thu,fri' start='22:00' end='07:00'/>
//!   <rule days='sat,sun' start='00:00' end='09:00'/>
//! </dnd>
//! ```
//!
//! * `timezone` — IANA tz database name (`chrono-tz` parses). Defaults
//!   to `UTC` when omitted. Required to make schedule evaluation
//!   deterministic across DST boundaries.
//! * `<snooze until='…'/>` — Optional; absolute RFC 3339 UTC instant.
//!   When present and in the future, DND is unconditionally active.
//! * `<rule …/>` — Zero or more weekly schedule rules. Each rule
//!   names one or more weekdays and a `start`/`end` `HH:MM` window in
//!   the document's timezone. Windows whose `end <= start` wrap past
//!   midnight into the following calendar day in the same tz.
//!
//! Unknown children and unknown attributes are rejected — clients
//! that want to extend the shape should bump the namespace.
//!
//! ## Evaluation
//!
//! [`WaddleDnd::evaluate`] takes a wall-clock instant (UTC) and
//! returns [`DndEvaluation::Active`] iff either (a) the snooze has
//! not yet elapsed OR (b) the instant falls inside a rule window in
//! the document's timezone. Otherwise [`DndEvaluation::Inactive`].
//!
//! The evaluator is pure (no I/O, no clock), so the per-XEP custom
//! test suite can exercise every weekday × DST × wrap-past-midnight
//! combination without spinning up storage or transport.

use std::str::FromStr;

use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use minidom::rxml::xml_ncname;
use minidom::Element;
use thiserror::Error;

/// Waddle DND namespace (also the PEP node name, per XEP-0163
/// single-payload-namespace convention).
///
/// Pinned equal to `waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_DND` so
/// `NodeConfig::pep_for_node` in the core crate can apply the
/// `whitelist + send-last-published=never` privacy defaults without
/// pulling `waddle-xmpp` into `waddle-xmpp-core`. The pin is exercised
/// by `pep_node_waddle_dnd_constant_matches_core` below.
pub const NS_WADDLE_DND_V0: &str = "urn:waddle:dnd:0";

/// PEP node name for DND state. By XEP-0163 convention the PEP node
/// for a single-payload-namespace projection equals the namespace.
pub const PEP_NODE_WADDLE_DND: &str = NS_WADDLE_DND_V0;

/// Conventional item id for single-item PEP nodes (XEP-0163 §"Item
/// Identifiers").
pub const ITEM_ID_CURRENT: &str = "current";

const ELEMENT_DND: &str = "dnd";
const ELEMENT_SNOOZE: &str = "snooze";
const ELEMENT_RULE: &str = "rule";

const ATTR_TIMEZONE: &str = "timezone";
const ATTR_UNTIL: &str = "until";
const ATTR_DAYS: &str = "days";
const ATTR_START: &str = "start";
const ATTR_END: &str = "end";

/// Cap on per-document rule count. A reasonable DND configuration has
/// one rule per workday cluster (weekday + weekend = 2). The cap
/// guards against a denial-of-evaluation publish that would blow up
/// the per-recipient T1 read.
pub const MAX_RULES_PER_DOCUMENT: usize = 16;

/// Parsed and validated DND state. The result of
/// [`parse`](WaddleDnd::parse) is guaranteed to be evaluable — all
/// fields are typed values, no stringly-typed back-doors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaddleDnd {
    pub timezone: Tz,
    pub snooze: Option<DateTime<Utc>>,
    pub rules: Vec<ScheduleRule>,
}

/// One weekly schedule rule. `days` is the set of weekdays the rule
/// fires on; `start` / `end` are wall-clock times in the document's
/// timezone. When `end <= start`, the window wraps past midnight into
/// the following calendar day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRule {
    pub days: WeekdaySet,
    pub start: NaiveTime,
    pub end: NaiveTime,
}

/// Typed set of weekdays, indexed `[Mon, Tue, Wed, Thu, Fri, Sat,
/// Sun]`. Backed by a fixed-size bool array so `chrono::Weekday`'s
/// missing `Ord`/`Hash` impls don't force us into a `Vec<Weekday>`
/// duplicate-check or a `BTreeSet` workaround.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WeekdaySet {
    members: [bool; 7],
}

impl WeekdaySet {
    pub fn new() -> Self {
        Self {
            members: [false; 7],
        }
    }

    pub fn insert(&mut self, day: Weekday) {
        self.members[weekday_index(day)] = true;
    }

    pub fn contains(&self, day: Weekday) -> bool {
        self.members[weekday_index(day)]
    }

    pub fn is_empty(&self) -> bool {
        self.members.iter().all(|present| !present)
    }

    pub fn iter(&self) -> impl Iterator<Item = Weekday> + '_ {
        WEEKDAYS_MON_TO_SUN
            .iter()
            .copied()
            .filter(|day| self.contains(*day))
    }
}

impl FromIterator<Weekday> for WeekdaySet {
    fn from_iter<I: IntoIterator<Item = Weekday>>(days: I) -> Self {
        let mut set = Self::new();
        for day in days {
            set.insert(day);
        }
        set
    }
}

fn weekday_index(day: Weekday) -> usize {
    day.num_days_from_monday() as usize
}

const WEEKDAYS_MON_TO_SUN: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

/// Result of evaluating a [`WaddleDnd`] state against a wall-clock
/// instant. Mirrors the server-side
/// `notification_outbox::DndState` enum so the integration boundary
/// is trivial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndEvaluation {
    Inactive,
    Active,
}

/// Errors raised during parse / build.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DndParseError {
    #[error("expected <dnd xmlns='{NS_WADDLE_DND_V0}'> root, got <{name} xmlns='{ns}'>")]
    WrongRoot { name: String, ns: String },
    #[error("unknown child element <{0}> in <dnd>")]
    UnknownChild(String),
    #[error("unknown attribute '{0}' on <dnd>")]
    UnknownAttribute(String),
    #[error("unknown attribute '{attr}' on <{element}>")]
    UnknownChildAttribute { element: String, attr: String },
    #[error("invalid IANA timezone: {0}")]
    InvalidTimezone(String),
    #[error("invalid RFC 3339 timestamp on <snooze until>: {0}")]
    InvalidSnoozeUntil(String),
    #[error("<snooze> requires the 'until' attribute")]
    SnoozeMissingUntil,
    #[error("more than one <snooze> child in <dnd>")]
    MultipleSnooze,
    #[error("<rule> requires the 'days' attribute")]
    RuleMissingDays,
    #[error("<rule> requires the 'start' attribute")]
    RuleMissingStart,
    #[error("<rule> requires the 'end' attribute")]
    RuleMissingEnd,
    #[error("invalid weekday token '{0}' in <rule days>")]
    InvalidWeekday(String),
    #[error("empty <rule days> attribute")]
    EmptyDays,
    #[error("invalid HH:MM time '{0}'")]
    InvalidTime(String),
    #[error("more than {MAX_RULES_PER_DOCUMENT} <rule> children in <dnd>")]
    TooManyRules,
    #[error("<rule start='{start}' end='{end}'> is a zero-length window")]
    ZeroLengthRule { start: String, end: String },
    #[error("namespaced attribute '{attr}' is not allowed on <{element}>")]
    NamespacedAttribute { element: String, attr: String },
    #[error("unexpected child element <{child}> in <{parent}>")]
    UnexpectedChild { parent: String, child: String },
}

impl WaddleDnd {
    /// Construct an Inactive (empty) DND state in UTC. Useful as the
    /// initial value when a user has never published a DND state.
    pub fn empty_utc() -> Self {
        Self {
            timezone: Tz::UTC,
            snooze: None,
            rules: Vec::new(),
        }
    }

    /// Parse a `<dnd xmlns='urn:waddle:dnd:0'>` element into a typed
    /// [`WaddleDnd`].
    pub fn parse(element: &Element) -> Result<Self, DndParseError> {
        if element.name() != ELEMENT_DND || element.ns() != NS_WADDLE_DND_V0 {
            return Err(DndParseError::WrongRoot {
                name: element.name().to_string(),
                ns: element.ns().to_string(),
            });
        }

        reject_unknown_attrs_on_root(element, &[ATTR_TIMEZONE])?;

        let timezone = match element.attr(ATTR_TIMEZONE) {
            Some(raw) => {
                Tz::from_str(raw).map_err(|_| DndParseError::InvalidTimezone(raw.to_string()))?
            }
            None => Tz::UTC,
        };

        let mut snooze: Option<DateTime<Utc>> = None;
        let mut rules: Vec<ScheduleRule> = Vec::new();

        for child in element.children() {
            if child.ns() != NS_WADDLE_DND_V0 {
                return Err(DndParseError::UnknownChild(child.name().to_string()));
            }
            match child.name() {
                ELEMENT_SNOOZE => {
                    if snooze.is_some() {
                        return Err(DndParseError::MultipleSnooze);
                    }
                    snooze = Some(parse_snooze(child)?);
                }
                ELEMENT_RULE => {
                    if rules.len() >= MAX_RULES_PER_DOCUMENT {
                        return Err(DndParseError::TooManyRules);
                    }
                    rules.push(parse_rule(child)?);
                }
                other => return Err(DndParseError::UnknownChild(other.to_string())),
            }
        }

        Ok(Self {
            timezone,
            snooze,
            rules,
        })
    }

    /// Serialize to a `<dnd>` element suitable for a PEP publish.
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder(ELEMENT_DND, NS_WADDLE_DND_V0)
            .attr(xml_ncname!("timezone").to_owned(), self.timezone.name());
        if let Some(until) = self.snooze {
            builder = builder.append(
                Element::builder(ELEMENT_SNOOZE, NS_WADDLE_DND_V0)
                    .attr(xml_ncname!("until").to_owned(), until.to_rfc3339()),
            );
        }
        for rule in &self.rules {
            builder = builder.append(rule.to_element());
        }
        builder.build()
    }

    /// Evaluate the DND state at the given wall-clock instant.
    ///
    /// Returns [`DndEvaluation::Active`] iff:
    /// * the snooze instant is set and `> now`, OR
    /// * at least one rule's window contains `now` in the document's
    ///   timezone (windows where `end <= start` wrap past midnight
    ///   into the following calendar day).
    pub fn evaluate(&self, now: DateTime<Utc>) -> DndEvaluation {
        if let Some(until) = self.snooze {
            if until > now {
                return DndEvaluation::Active;
            }
        }
        let local = now.with_timezone(&self.timezone);
        for rule in &self.rules {
            if rule_contains(rule, &local) {
                return DndEvaluation::Active;
            }
        }
        DndEvaluation::Inactive
    }
}

impl ScheduleRule {
    fn to_element(&self) -> Element {
        Element::builder(ELEMENT_RULE, NS_WADDLE_DND_V0)
            .attr(
                xml_ncname!("days").to_owned(),
                weekday_set_to_string(&self.days),
            )
            .attr(xml_ncname!("start").to_owned(), format_time(self.start))
            .attr(xml_ncname!("end").to_owned(), format_time(self.end))
            .build()
    }

    /// Returns true when the window wraps past midnight.
    ///
    /// Strict less-than: `end == start` is a zero-length rule rejected
    /// at parse time (see [`DndParseError::ZeroLengthRule`]). Without
    /// the rejection, a degenerate `start='22:00' end='22:00'` would
    /// silently expand to a 24-hour DND window via the wrap branch.
    pub fn wraps_past_midnight(&self) -> bool {
        self.end < self.start
    }
}

fn reject_unknown_attrs_on_root(element: &Element, known: &[&str]) -> Result<(), DndParseError> {
    for ((ns, name), _value) in element.attrs().iter() {
        let attr_name = name.as_str();
        // Prefixed attributes (non-empty namespace) are never part of
        // this XEP's contract — reject them outright so a client can't
        // sneak `foo:timezone='bar'` past the strict-parser gate by
        // riding a different namespace.
        if !ns.as_str().is_empty() {
            return Err(DndParseError::NamespacedAttribute {
                element: ELEMENT_DND.to_string(),
                attr: attr_name.to_string(),
            });
        }
        if !known.contains(&attr_name) {
            return Err(DndParseError::UnknownAttribute(attr_name.to_string()));
        }
    }
    Ok(())
}

fn reject_unknown_attrs_on_child(
    element: &Element,
    element_name: &str,
    known: &[&str],
) -> Result<(), DndParseError> {
    for ((ns, name), _value) in element.attrs().iter() {
        let attr_name = name.as_str();
        if !ns.as_str().is_empty() {
            return Err(DndParseError::NamespacedAttribute {
                element: element_name.to_string(),
                attr: attr_name.to_string(),
            });
        }
        if !known.contains(&attr_name) {
            return Err(DndParseError::UnknownChildAttribute {
                element: element_name.to_string(),
                attr: attr_name.to_string(),
            });
        }
    }
    Ok(())
}

fn reject_any_children(element: &Element, parent: &str) -> Result<(), DndParseError> {
    if let Some(child) = element.children().next() {
        return Err(DndParseError::UnexpectedChild {
            parent: parent.to_string(),
            child: child.name().to_string(),
        });
    }
    Ok(())
}

fn parse_snooze(element: &Element) -> Result<DateTime<Utc>, DndParseError> {
    reject_unknown_attrs_on_child(element, ELEMENT_SNOOZE, &[ATTR_UNTIL])?;
    reject_any_children(element, ELEMENT_SNOOZE)?;
    let raw = element
        .attr(ATTR_UNTIL)
        .ok_or(DndParseError::SnoozeMissingUntil)?;
    let parsed = DateTime::parse_from_rfc3339(raw)
        .map_err(|_| DndParseError::InvalidSnoozeUntil(raw.to_string()))?;
    Ok(parsed.with_timezone(&Utc))
}

fn parse_rule(element: &Element) -> Result<ScheduleRule, DndParseError> {
    reject_unknown_attrs_on_child(element, ELEMENT_RULE, &[ATTR_DAYS, ATTR_START, ATTR_END])?;
    reject_any_children(element, ELEMENT_RULE)?;
    let days_attr = element
        .attr(ATTR_DAYS)
        .ok_or(DndParseError::RuleMissingDays)?;
    let start_attr = element
        .attr(ATTR_START)
        .ok_or(DndParseError::RuleMissingStart)?;
    let end_attr = element
        .attr(ATTR_END)
        .ok_or(DndParseError::RuleMissingEnd)?;
    let days = parse_weekday_set(days_attr)?;
    let start = parse_time(start_attr)?;
    let end = parse_time(end_attr)?;
    if start == end {
        return Err(DndParseError::ZeroLengthRule {
            start: start_attr.to_string(),
            end: end_attr.to_string(),
        });
    }
    Ok(ScheduleRule { days, start, end })
}

fn parse_weekday_set(raw: &str) -> Result<WeekdaySet, DndParseError> {
    let mut set = WeekdaySet::new();
    let mut saw_any = false;
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        saw_any = true;
        set.insert(parse_weekday(trimmed)?);
    }
    if !saw_any {
        return Err(DndParseError::EmptyDays);
    }
    Ok(set)
}

fn parse_weekday(token: &str) -> Result<Weekday, DndParseError> {
    match token.to_ascii_lowercase().as_str() {
        "mon" => Ok(Weekday::Mon),
        "tue" => Ok(Weekday::Tue),
        "wed" => Ok(Weekday::Wed),
        "thu" => Ok(Weekday::Thu),
        "fri" => Ok(Weekday::Fri),
        "sat" => Ok(Weekday::Sat),
        "sun" => Ok(Weekday::Sun),
        _ => Err(DndParseError::InvalidWeekday(token.to_string())),
    }
}

fn weekday_to_token(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

fn weekday_set_to_string(set: &WeekdaySet) -> String {
    let mut out = String::new();
    for day in set.iter() {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(weekday_to_token(day));
    }
    out
}

fn parse_time(raw: &str) -> Result<NaiveTime, DndParseError> {
    NaiveTime::parse_from_str(raw, "%H:%M").map_err(|_| DndParseError::InvalidTime(raw.to_string()))
}

fn format_time(time: NaiveTime) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

/// Returns true if `local` falls inside `rule`'s window.
///
/// A non-wrapping rule (`start < end`) fires on each listed weekday
/// between `start` (inclusive) and `end` (exclusive).
///
/// A wrapping rule (`end <= start`) fires from `start` (inclusive) on
/// each listed weekday through `end` (exclusive) on the FOLLOWING
/// day. Checking `local` therefore requires looking at both:
///   * "Is today a listed day AND `local.time() >= start`?", and
///   * "Was yesterday a listed day AND `local.time() < end`?".
fn rule_contains<Tz: TimeZone>(rule: &ScheduleRule, local: &DateTime<Tz>) -> bool {
    let today = local.weekday();
    let yesterday = today.pred();
    let now_time = local.time();
    if rule.wraps_past_midnight() {
        if rule.days.contains(today) && now_time >= rule.start {
            return true;
        }
        if rule.days.contains(yesterday) && now_time < rule.end {
            return true;
        }
        false
    } else {
        rule.days.contains(today) && now_time >= rule.start && now_time < rule.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use minidom::Element;

    fn dnd_xml(inner: &str) -> Element {
        let raw = format!("<dnd xmlns='{NS_WADDLE_DND_V0}'>{inner}</dnd>");
        raw.parse().expect("test fixture must be valid XML")
    }

    fn dnd_with_attrs(attrs: &str, inner: &str) -> Element {
        let raw = format!("<dnd xmlns='{NS_WADDLE_DND_V0}' {attrs}>{inner}</dnd>");
        raw.parse().expect("test fixture must be valid XML")
    }

    fn weekdays(days: &[Weekday]) -> WeekdaySet {
        WeekdaySet::from_iter(days.iter().copied())
    }

    fn ts(year: i32, mon: u32, day: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, mon, day, h, m, 0).unwrap()
    }

    #[test]
    fn parse_empty_dnd_defaults_to_utc_no_snooze_no_rules() {
        let parsed = WaddleDnd::parse(&dnd_xml("")).expect("empty dnd is valid");
        assert_eq!(parsed.timezone, Tz::UTC);
        assert!(parsed.snooze.is_none());
        assert!(parsed.rules.is_empty());
    }

    #[test]
    fn parse_round_trip_preserves_typed_state() {
        let input = WaddleDnd {
            timezone: Tz::Europe__Oslo,
            snooze: Some(ts(2026, 5, 23, 17, 0)),
            rules: vec![
                ScheduleRule {
                    days: weekdays(&[
                        Weekday::Mon,
                        Weekday::Tue,
                        Weekday::Wed,
                        Weekday::Thu,
                        Weekday::Fri,
                    ]),
                    start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                    end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
                },
                ScheduleRule {
                    days: weekdays(&[Weekday::Sat, Weekday::Sun]),
                    start: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
                    end: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                },
            ],
        };
        let element = input.to_element();
        let parsed = WaddleDnd::parse(&element).expect("round-trip parse");
        assert_eq!(parsed, input);
    }

    #[test]
    fn parse_unknown_root_element_rejected() {
        let bad: Element = "<other xmlns='urn:waddle:dnd:0'/>".parse().unwrap();
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::WrongRoot { .. })
        ));
    }

    #[test]
    fn parse_unknown_root_namespace_rejected() {
        let bad: Element = "<dnd xmlns='urn:example:other'/>".parse().unwrap();
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::WrongRoot { .. })
        ));
    }

    #[test]
    fn parse_unknown_child_rejected() {
        let bad = dnd_xml("<weird/>");
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::UnknownChild(_))
        ));
    }

    #[test]
    fn parse_unknown_attribute_rejected() {
        let bad = dnd_with_attrs("foo='bar'", "");
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::UnknownAttribute(_))
        ));
    }

    #[test]
    fn parse_invalid_timezone_rejected() {
        let bad = dnd_with_attrs("timezone='Not/A/Real_Zone'", "");
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::InvalidTimezone(_))
        ));
    }

    #[test]
    fn parse_multiple_snooze_rejected() {
        let bad =
            dnd_xml("<snooze until='2026-05-23T17:00:00Z'/><snooze until='2026-05-23T18:00:00Z'/>");
        assert_eq!(
            WaddleDnd::parse(&bad).unwrap_err(),
            DndParseError::MultipleSnooze
        );
    }

    #[test]
    fn parse_snooze_missing_until_rejected() {
        let bad = dnd_xml("<snooze/>");
        assert_eq!(
            WaddleDnd::parse(&bad).unwrap_err(),
            DndParseError::SnoozeMissingUntil
        );
    }

    #[test]
    fn parse_invalid_snooze_until_rejected() {
        let bad = dnd_xml("<snooze until='not-a-timestamp'/>");
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::InvalidSnoozeUntil(_))
        ));
    }

    #[test]
    fn parse_rule_missing_days_rejected() {
        let bad = dnd_xml("<rule start='22:00' end='07:00'/>");
        assert_eq!(
            WaddleDnd::parse(&bad).unwrap_err(),
            DndParseError::RuleMissingDays
        );
    }

    #[test]
    fn parse_rule_missing_start_rejected() {
        let bad = dnd_xml("<rule days='mon' end='07:00'/>");
        assert_eq!(
            WaddleDnd::parse(&bad).unwrap_err(),
            DndParseError::RuleMissingStart
        );
    }

    #[test]
    fn parse_rule_missing_end_rejected() {
        let bad = dnd_xml("<rule days='mon' start='22:00'/>");
        assert_eq!(
            WaddleDnd::parse(&bad).unwrap_err(),
            DndParseError::RuleMissingEnd
        );
    }

    #[test]
    fn parse_invalid_weekday_rejected() {
        let bad = dnd_xml("<rule days='mon,xyz' start='22:00' end='07:00'/>");
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::InvalidWeekday(_))
        ));
    }

    #[test]
    fn parse_empty_days_rejected() {
        let bad = dnd_xml("<rule days='' start='22:00' end='07:00'/>");
        assert_eq!(
            WaddleDnd::parse(&bad).unwrap_err(),
            DndParseError::EmptyDays
        );
    }

    #[test]
    fn parse_invalid_time_rejected() {
        let bad = dnd_xml("<rule days='mon' start='25:00' end='07:00'/>");
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::InvalidTime(_))
        ));
    }

    #[test]
    fn parse_too_many_rules_rejected() {
        let mut inner = String::new();
        for _ in 0..=MAX_RULES_PER_DOCUMENT {
            inner.push_str("<rule days='mon' start='22:00' end='23:00'/>");
        }
        let bad = dnd_xml(&inner);
        assert_eq!(
            WaddleDnd::parse(&bad).unwrap_err(),
            DndParseError::TooManyRules
        );
    }

    #[test]
    fn parse_namespaced_attribute_on_root_rejected() {
        // `foo:timezone='X'` must not silently fall through the
        // local-name check — the strict-parser contract requires
        // rejecting attributes carrying a non-empty namespace.
        let bad: Element = "<dnd xmlns='urn:waddle:dnd:0' \
             xmlns:other='urn:example:other' other:timezone='X'/>"
            .parse()
            .unwrap();
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::NamespacedAttribute { .. })
        ));
    }

    #[test]
    fn parse_namespaced_attribute_on_snooze_rejected() {
        let bad: Element = "<dnd xmlns='urn:waddle:dnd:0'>\
             <snooze xmlns:other='urn:example:other' \
                     until='2026-05-23T17:00:00Z' \
                     other:until='whatever'/>\
             </dnd>"
            .parse()
            .unwrap();
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::NamespacedAttribute { .. })
        ));
    }

    #[test]
    fn parse_unknown_child_in_snooze_rejected() {
        let bad: Element =
            "<dnd xmlns='urn:waddle:dnd:0'><snooze until='2026-05-23T17:00:00Z'><extra/></snooze></dnd>"
                .parse()
                .unwrap();
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::UnexpectedChild { .. })
        ));
    }

    #[test]
    fn parse_unknown_child_in_rule_rejected() {
        let bad: Element =
            "<dnd xmlns='urn:waddle:dnd:0'><rule days='mon' start='22:00' end='07:00'><extra/></rule></dnd>"
                .parse()
                .unwrap();
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::UnexpectedChild { .. })
        ));
    }

    #[test]
    fn parse_zero_length_rule_rejected() {
        let bad: Element =
            "<dnd xmlns='urn:waddle:dnd:0'><rule days='mon' start='22:00' end='22:00'/></dnd>"
                .parse()
                .unwrap();
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::ZeroLengthRule { .. })
        ));
    }

    #[test]
    fn parse_unknown_attribute_on_snooze_rejected() {
        let bad = dnd_xml("<snooze until='2026-05-23T17:00:00Z' foo='bar'/>");
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::UnknownChildAttribute { .. })
        ));
    }

    #[test]
    fn parse_unknown_attribute_on_rule_rejected() {
        let bad = dnd_xml("<rule days='mon' start='22:00' end='23:00' foo='bar'/>");
        assert!(matches!(
            WaddleDnd::parse(&bad),
            Err(DndParseError::UnknownChildAttribute { .. })
        ));
    }

    #[test]
    fn evaluate_empty_state_is_inactive() {
        let state = WaddleDnd::empty_utc();
        assert_eq!(
            state.evaluate(ts(2026, 5, 23, 12, 0)),
            DndEvaluation::Inactive
        );
    }

    #[test]
    fn evaluate_snooze_in_future_is_active() {
        let state = WaddleDnd {
            timezone: Tz::UTC,
            snooze: Some(ts(2026, 5, 23, 17, 0)),
            rules: vec![],
        };
        assert_eq!(
            state.evaluate(ts(2026, 5, 23, 16, 59)),
            DndEvaluation::Active
        );
    }

    #[test]
    fn evaluate_snooze_at_or_past_deadline_falls_through_to_rules() {
        // At exactly the deadline, the snooze has elapsed (Active when
        // strictly `until > now`). With no rules, the state collapses
        // to Inactive — the snooze does not "linger" at its boundary.
        let state = WaddleDnd {
            timezone: Tz::UTC,
            snooze: Some(ts(2026, 5, 23, 17, 0)),
            rules: vec![],
        };
        assert_eq!(
            state.evaluate(ts(2026, 5, 23, 17, 0)),
            DndEvaluation::Inactive
        );
        let past = Utc.with_ymd_and_hms(2026, 5, 23, 17, 0, 1).unwrap();
        assert_eq!(state.evaluate(past), DndEvaluation::Inactive);
    }

    #[test]
    fn evaluate_non_wrapping_rule_inside_window_active() {
        let state = WaddleDnd {
            timezone: Tz::UTC,
            snooze: None,
            rules: vec![ScheduleRule {
                days: weekdays(&[Weekday::Wed]),
                start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
            }],
        };
        // 2026-05-20 is a Wednesday.
        assert_eq!(
            state.evaluate(ts(2026, 5, 20, 12, 0)),
            DndEvaluation::Active
        );
        assert_eq!(
            state.evaluate(ts(2026, 5, 20, 8, 59)),
            DndEvaluation::Inactive
        );
        // end is exclusive
        assert_eq!(
            state.evaluate(ts(2026, 5, 20, 17, 0)),
            DndEvaluation::Inactive
        );
        // wrong day
        assert_eq!(
            state.evaluate(ts(2026, 5, 21, 12, 0)),
            DndEvaluation::Inactive
        );
    }

    #[test]
    fn evaluate_wrap_past_midnight_late_evening_active() {
        // Mon 22:00 → Tue 07:00 in Oslo. 2026-05-25 is a Monday.
        let state = WaddleDnd {
            timezone: Tz::Europe__Oslo,
            snooze: None,
            rules: vec![ScheduleRule {
                days: weekdays(&[Weekday::Mon]),
                start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            }],
        };
        // 2026-05-25 23:00 Oslo = 21:00 UTC (CEST is UTC+2 in May).
        assert_eq!(
            state.evaluate(ts(2026, 5, 25, 21, 0)),
            DndEvaluation::Active
        );
    }

    #[test]
    fn evaluate_wrap_past_midnight_early_morning_following_day_active() {
        let state = WaddleDnd {
            timezone: Tz::Europe__Oslo,
            snooze: None,
            rules: vec![ScheduleRule {
                days: weekdays(&[Weekday::Mon]),
                start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            }],
        };
        // Tuesday 2026-05-26 03:00 Oslo = 01:00 UTC. Yesterday (Mon)
        // is in the rule's days; time < end ⇒ Active.
        assert_eq!(state.evaluate(ts(2026, 5, 26, 1, 0)), DndEvaluation::Active);
    }

    #[test]
    fn evaluate_wrap_past_midnight_far_outside_window_inactive() {
        let state = WaddleDnd {
            timezone: Tz::Europe__Oslo,
            snooze: None,
            rules: vec![ScheduleRule {
                days: weekdays(&[Weekday::Mon]),
                start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            }],
        };
        // Monday 14:00 Oslo = 12:00 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 5, 25, 12, 0)),
            DndEvaluation::Inactive
        );
        // Tuesday 14:00 Oslo (after the window ends).
        assert_eq!(
            state.evaluate(ts(2026, 5, 26, 12, 0)),
            DndEvaluation::Inactive
        );
    }

    #[test]
    fn evaluate_dst_spring_forward_oslo_2026() {
        // 2026-03-29 is the spring-forward DST jump in Oslo: 02:00
        // local does not exist (clocks jump 02→03). A rule at 21:00→
        // 06:00 Sat→Sun MUST still fire at the equivalent UTC
        // instants. 2026-03-28 is a Saturday.
        let state = WaddleDnd {
            timezone: Tz::Europe__Oslo,
            snooze: None,
            rules: vec![ScheduleRule {
                days: weekdays(&[Weekday::Sat]),
                start: NaiveTime::from_hms_opt(21, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
            }],
        };
        // Sat 22:00 Oslo (CET = UTC+1) = 21:00 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 3, 28, 21, 0)),
            DndEvaluation::Active
        );
        // Sun 05:00 Oslo AFTER the jump (CEST = UTC+2) = 03:00 UTC.
        assert_eq!(state.evaluate(ts(2026, 3, 29, 3, 0)), DndEvaluation::Active);
        // Sun 07:00 Oslo = 05:00 UTC (post-window end).
        assert_eq!(
            state.evaluate(ts(2026, 3, 29, 5, 0)),
            DndEvaluation::Inactive
        );
    }

    #[test]
    fn evaluate_dst_fall_back_oslo_2026() {
        // 2026-10-25 is the fall-back DST jump in Oslo: 03:00 local
        // happens twice. A rule 22:00 Sat → 07:00 Sun MUST still
        // suppress through both 02:30 instances.
        let state = WaddleDnd {
            timezone: Tz::Europe__Oslo,
            snooze: None,
            rules: vec![ScheduleRule {
                days: weekdays(&[Weekday::Sat]),
                start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            }],
        };
        // Sat 2026-10-24 23:00 Oslo CEST = 21:00 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 10, 24, 21, 0)),
            DndEvaluation::Active
        );
        // Sun 2026-10-25 first 02:30 Oslo CEST = 00:30 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 10, 25, 0, 30)),
            DndEvaluation::Active
        );
        // Sun 2026-10-25 second 02:30 Oslo CET = 01:30 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 10, 25, 1, 30)),
            DndEvaluation::Active
        );
        // Sun 2026-10-25 08:00 Oslo CET = 07:00 UTC (past window end).
        assert_eq!(
            state.evaluate(ts(2026, 10, 25, 7, 0)),
            DndEvaluation::Inactive
        );
    }

    #[test]
    fn evaluate_multi_rule_any_match_wins() {
        let state = WaddleDnd {
            timezone: Tz::UTC,
            snooze: None,
            rules: vec![
                ScheduleRule {
                    days: weekdays(&[Weekday::Mon]),
                    start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                    end: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
                },
                ScheduleRule {
                    days: weekdays(&[Weekday::Wed]),
                    start: NaiveTime::from_hms_opt(14, 0, 0).unwrap(),
                    end: NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
                },
            ],
        };
        // 2026-05-20 is Wednesday, 14:30 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 5, 20, 14, 30)),
            DndEvaluation::Active
        );
    }

    #[test]
    fn evaluate_pacific_auckland_wraps_correctly() {
        // Friday night → Saturday morning in Auckland. 2026-05-22
        // is Friday; Auckland in May is NZST = UTC+12.
        let state = WaddleDnd {
            timezone: Tz::Pacific__Auckland,
            snooze: None,
            rules: vec![ScheduleRule {
                days: weekdays(&[Weekday::Fri]),
                start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
            }],
        };
        // Fri 2026-05-22 23:00 Auckland = 11:00 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 5, 22, 11, 0)),
            DndEvaluation::Active
        );
        // Sat 2026-05-23 03:00 Auckland = Fri 2026-05-22 15:00 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 5, 22, 15, 0)),
            DndEvaluation::Active
        );
        // Sat 2026-05-23 07:00 Auckland = Fri 2026-05-22 19:00 UTC.
        assert_eq!(
            state.evaluate(ts(2026, 5, 22, 19, 0)),
            DndEvaluation::Inactive
        );
    }

    #[test]
    fn snooze_active_takes_precedence_when_no_rules_match() {
        let state = WaddleDnd {
            timezone: Tz::UTC,
            snooze: Some(ts(2026, 5, 23, 17, 0)),
            rules: vec![],
        };
        assert_eq!(
            state.evaluate(ts(2026, 5, 23, 16, 30)),
            DndEvaluation::Active
        );
    }

    #[test]
    fn weekday_set_serialization_is_canonical_mon_to_sun_order() {
        let rule = ScheduleRule {
            days: weekdays(&[Weekday::Sun, Weekday::Mon, Weekday::Wed]),
            start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
        };
        let element = rule.to_element();
        assert_eq!(element.attr(ATTR_DAYS), Some("mon,wed,sun"));
    }

    #[test]
    fn weekday_set_iter_yields_mon_to_sun_order() {
        let set = weekdays(&[Weekday::Sun, Weekday::Fri, Weekday::Mon]);
        let collected: Vec<Weekday> = set.iter().collect();
        assert_eq!(collected, vec![Weekday::Mon, Weekday::Fri, Weekday::Sun]);
    }

    /// Pin the `waddle-xmpp`-side namespace constant to its sibling in
    /// `waddle-xmpp-core` so a rename in either crate fails CI.
    #[test]
    fn pep_node_waddle_dnd_constant_matches_core() {
        assert_eq!(
            PEP_NODE_WADDLE_DND,
            waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_DND
        );
    }

    #[test]
    fn weekday_set_contains_only_inserted_members() {
        let set = weekdays(&[Weekday::Tue, Weekday::Thu]);
        assert!(set.contains(Weekday::Tue));
        assert!(set.contains(Weekday::Thu));
        assert!(!set.contains(Weekday::Wed));
        assert!(!set.contains(Weekday::Sat));
    }
}
