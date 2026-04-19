//! XEP-0108: User Activity.
//!
//! Publishes the user's current activity over PEP
//! (`http://jabber.org/protocol/activity`). Activities have a two-level
//! taxonomy: a required general category (e.g. `working`) and an optional
//! specific sub-activity (e.g. `coding`).

use minidom::Element;
use thiserror::Error;

/// Payload / PEP node namespace.
pub const NS_ACTIVITY: &str = "http://jabber.org/protocol/activity";
/// PEP node used for publication.
pub const PEP_NODE_ACTIVITY: &str = "http://jabber.org/protocol/activity";

/// General activity categories (XEP-0108 §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeneralActivity {
    DoingChores,
    Drinking,
    Eating,
    Exercising,
    Grooming,
    HavingAppointment,
    Inactive,
    Relaxing,
    Talking,
    Traveling,
    Working,
    Undefined,
}

impl GeneralActivity {
    pub fn as_element_name(&self) -> &'static str {
        match self {
            GeneralActivity::DoingChores => "doing_chores",
            GeneralActivity::Drinking => "drinking",
            GeneralActivity::Eating => "eating",
            GeneralActivity::Exercising => "exercising",
            GeneralActivity::Grooming => "grooming",
            GeneralActivity::HavingAppointment => "having_appointment",
            GeneralActivity::Inactive => "inactive",
            GeneralActivity::Relaxing => "relaxing",
            GeneralActivity::Talking => "talking",
            GeneralActivity::Traveling => "traveling",
            GeneralActivity::Working => "working",
            GeneralActivity::Undefined => "undefined",
        }
    }

    pub fn from_element_name(name: &str) -> Option<GeneralActivity> {
        Some(match name {
            "doing_chores" => GeneralActivity::DoingChores,
            "drinking" => GeneralActivity::Drinking,
            "eating" => GeneralActivity::Eating,
            "exercising" => GeneralActivity::Exercising,
            "grooming" => GeneralActivity::Grooming,
            "having_appointment" => GeneralActivity::HavingAppointment,
            "inactive" => GeneralActivity::Inactive,
            "relaxing" => GeneralActivity::Relaxing,
            "talking" => GeneralActivity::Talking,
            "traveling" => GeneralActivity::Traveling,
            "working" => GeneralActivity::Working,
            "undefined" => GeneralActivity::Undefined,
            _ => return None,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ActivityError {
    #[error("expected <activity/> in '{NS_ACTIVITY}'")]
    WrongElement,
    #[error("missing general activity element")]
    MissingGeneral,
    #[error("unknown general activity '{0}'")]
    UnknownGeneral(String),
}

/// Typed activity publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub general: GeneralActivity,
    /// Optional specific sub-activity element name (e.g. `coding`, `studying`).
    /// The XEP enumerates many; we treat the string as a typed newtype below
    /// to preserve extensibility without allowing arbitrary payloads.
    pub specific: Option<SpecificActivity>,
    pub text: Option<String>,
}

/// A specific activity is a well-known element name under the general
/// category. We wrap it in a newtype so that callers must go through
/// `SpecificActivity::new` which asserts it is XML-safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecificActivity(String);

impl SpecificActivity {
    /// Create a specific activity from a snake_case identifier.
    ///
    /// Returns `None` for empty strings or names containing characters that
    /// are invalid in XML element names.
    pub fn new(name: impl Into<String>) -> Option<SpecificActivity> {
        let s = name.into();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return None;
        }
        Some(SpecificActivity(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Activity {
    pub fn new(general: GeneralActivity) -> Self {
        Self {
            general,
            specific: None,
            text: None,
        }
    }

    pub fn with_specific(mut self, specific: SpecificActivity) -> Self {
        self.specific = Some(specific);
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// Build a typed `<activity/>` element.
pub fn build_activity_element(activity: &Activity) -> Element {
    let mut general_builder = Element::builder(activity.general.as_element_name(), NS_ACTIVITY);
    if let Some(specific) = &activity.specific {
        general_builder =
            general_builder.append(Element::builder(specific.as_str(), NS_ACTIVITY).build());
    }
    let mut builder = Element::builder("activity", NS_ACTIVITY).append(general_builder.build());
    if let Some(text) = &activity.text {
        builder = builder.append(
            Element::builder("text", NS_ACTIVITY)
                .append(text.as_str())
                .build(),
        );
    }
    builder.build()
}

/// Build the empty `<activity/>` retraction element.
pub fn build_activity_retraction() -> Element {
    Element::builder("activity", NS_ACTIVITY).build()
}

/// Parse a `<activity/>` element. Returns `Ok(None)` for the empty retraction
/// form.
pub fn parse_activity_element(elem: &Element) -> Result<Option<Activity>, ActivityError> {
    if !elem.is("activity", NS_ACTIVITY) {
        return Err(ActivityError::WrongElement);
    }
    let mut general: Option<GeneralActivity> = None;
    let mut specific: Option<SpecificActivity> = None;
    let mut text: Option<String> = None;
    let mut has_any = false;
    for child in elem.children() {
        if child.ns() != NS_ACTIVITY {
            continue;
        }
        has_any = true;
        let name = child.name();
        if name == "text" {
            text = Some(child.text());
            continue;
        }
        if general.is_some() {
            continue;
        }
        match GeneralActivity::from_element_name(name) {
            Some(g) => {
                general = Some(g);
                for sub in child.children() {
                    if sub.ns() != NS_ACTIVITY {
                        continue;
                    }
                    if let Some(sa) = SpecificActivity::new(sub.name().to_string()) {
                        specific = Some(sa);
                    }
                }
            }
            None => return Err(ActivityError::UnknownGeneral(name.to_string())),
        }
    }
    if !has_any {
        return Ok(None);
    }
    let general = general.ok_or(ActivityError::MissingGeneral)?;
    Ok(Some(Activity {
        general,
        specific,
        text,
    }))
}

pub fn is_activity_element(elem: &Element) -> bool {
    elem.is("activity", NS_ACTIVITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip_general_only() {
        let a = Activity::new(GeneralActivity::Working);
        let elem = build_activity_element(&a);
        let parsed = parse_activity_element(&elem).unwrap().unwrap();
        assert_eq!(parsed, a);
    }

    #[test]
    fn test_round_trip_with_specific_and_text() {
        let a = Activity::new(GeneralActivity::Working)
            .with_specific(SpecificActivity::new("coding").unwrap())
            .with_text("Rewriting MIX");
        let elem = build_activity_element(&a);
        let parsed = parse_activity_element(&elem).unwrap().unwrap();
        assert_eq!(parsed, a);
    }

    #[test]
    fn test_retraction_empty() {
        let elem = build_activity_retraction();
        assert!(parse_activity_element(&elem).unwrap().is_none());
    }

    #[test]
    fn test_unknown_general_rejected() {
        let elem = Element::builder("activity", NS_ACTIVITY)
            .append(Element::builder("flying", NS_ACTIVITY).build())
            .build();
        assert_eq!(
            parse_activity_element(&elem),
            Err(ActivityError::UnknownGeneral("flying".into()))
        );
    }

    #[test]
    fn test_wrong_element() {
        let elem = Element::builder("activity", "other").build();
        assert_eq!(
            parse_activity_element(&elem),
            Err(ActivityError::WrongElement)
        );
    }

    #[test]
    fn test_specific_newtype_rejects_invalid() {
        assert!(SpecificActivity::new("").is_none());
        assert!(SpecificActivity::new("Has Space").is_none());
        assert!(SpecificActivity::new("UPPER").is_none());
        assert!(SpecificActivity::new("coding").is_some());
        assert!(SpecificActivity::new("doing_a_thing").is_some());
    }

    #[test]
    fn test_is_activity_element() {
        assert!(is_activity_element(
            &Element::builder("activity", NS_ACTIVITY).build()
        ));
        assert!(!is_activity_element(
            &Element::builder("mood", NS_ACTIVITY).build()
        ));
    }
}
