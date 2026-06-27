//! Waddle status preference: the user's manually-picked presence mode,
//! synced across their own devices (ADR-010 Phase 4).
//!
//! Phase 1 stores the chosen mode in-memory on one device only. This
//! payload persists it as a single pubsub item on the user's own JID
//! per XEP-0223 "Persistent Storage of Private Data via PubSub", so the
//! pick follows the user across resources and survives a reconnect. No
//! XEP models a "synced chosen-presence", so the payload lives in the
//! project-local `urn:waddle:status-preference:0` namespace per the
//! CLAUDE.md XEP-conformance hard rule. The server-side transport
//! conventions mirror `urn:waddle:dnd:0` (owner-only, `whitelist`
//! access, single item id `current`).
//!
//! Auto-away (Phase 2) is deliberately NOT part of this payload — it
//! stays per-device.
//!
//! ## XML Format
//!
//! ```xml
//! <status-preference xmlns='urn:waddle:status-preference:0' mode='automatic'/>
//! <status-preference xmlns='urn:waddle:status-preference:0' mode='manual' status='away'/>
//! ```
//!
//! * `mode` — required, `automatic` | `manual`.
//! * `status` — required iff `mode='manual'`, one of `available` |
//!   `chat` | `away` | `dnd` (the pickable manual-status set). Forbidden
//!   when `mode='automatic'`.
//!
//! Reset-to-automatic is published as an explicit `mode='automatic'`
//! item rather than a retract: the node defaults pin `notify_retract =
//! false` (mirroring dnd), so a retract would not fan out to the user's
//! other online resources. An explicit publish makes every mode change
//! — including reset — a normal publish that the XEP-0163 §3.4 self
//! fan-out delivers live.
//!
//! The parser is strict (dnd style): unknown attributes, child
//! elements, text content, and namespaced attributes are rejected so a
//! client that wants to extend the shape must bump the namespace.

use minidom::rxml::xml_ncname;
use minidom::Element;
use thiserror::Error;

/// XML namespace + PEP node id for the status-preference payload. They
/// coincide deliberately, matching XEP-0163's one-node-per-namespace
/// convention; the two constants exist for call-site readability.
pub const NS_WADDLE_STATUS_PREFERENCE: &str = "urn:waddle:status-preference:0";

/// PEP node id for the status-preference payload. Equal to
/// [`NS_WADDLE_STATUS_PREFERENCE`] by design.
pub const PEP_NODE_WADDLE_STATUS_PREFERENCE: &str = NS_WADDLE_STATUS_PREFERENCE;

/// Single fixed item id (the XEP-0163 single-item idiom), overwritten
/// in place on every publish.
pub const PEP_ITEM_WADDLE_STATUS_PREFERENCE: &str = "current";

const ELEMENT_STATUS_PREFERENCE: &str = "status-preference";
const ATTR_MODE: &str = "mode";
const ATTR_STATUS: &str = "status";

const MODE_AUTOMATIC: &str = "automatic";
const MODE_MANUAL: &str = "manual";

const STATUS_AVAILABLE: &str = "available";
const STATUS_CHAT: &str = "chat";
const STATUS_AWAY: &str = "away";
const STATUS_DND: &str = "dnd";

/// The user's manually-picked status when not in Automatic mode. Mirrors
/// the chat client's `ManualStatus` (`available | chat | away | dnd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualStatus {
    Available,
    /// RFC 6121 `<show>chat</show>` — "free for chat" (ADR-010 Phase 5b).
    Chat,
    Away,
    Dnd,
}

impl ManualStatus {
    /// The wire token for this status (`available` | `chat` | `away` | `dnd`).
    pub fn as_str(self) -> &'static str {
        match self {
            ManualStatus::Available => STATUS_AVAILABLE,
            ManualStatus::Chat => STATUS_CHAT,
            ManualStatus::Away => STATUS_AWAY,
            ManualStatus::Dnd => STATUS_DND,
        }
    }

    /// Parse a wire token into a [`ManualStatus`].
    pub fn from_token(raw: &str) -> Result<Self, StatusPreferenceParseError> {
        match raw {
            STATUS_AVAILABLE => Ok(ManualStatus::Available),
            STATUS_CHAT => Ok(ManualStatus::Chat),
            STATUS_AWAY => Ok(ManualStatus::Away),
            STATUS_DND => Ok(ManualStatus::Dnd),
            other => Err(StatusPreferenceParseError::UnknownStatus(other.to_string())),
        }
    }
}

/// The user's chosen presence mode. `Automatic` lets the per-device
/// auto-away timer govern; `Manual` pins a specific status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPreference {
    Automatic,
    Manual(ManualStatus),
}

/// Errors raised while parsing a `<status-preference>` element.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StatusPreferenceParseError {
    #[error(
        "expected <status-preference xmlns='{NS_WADDLE_STATUS_PREFERENCE}'> root, \
         got <{name} xmlns='{ns}'>"
    )]
    WrongRoot { name: String, ns: String },
    #[error("<status-preference> requires the 'mode' attribute")]
    MissingMode,
    #[error("unknown mode '{0}' (expected 'automatic' or 'manual')")]
    UnknownMode(String),
    #[error("mode='manual' requires the 'status' attribute")]
    MissingStatus,
    #[error("'status' attribute is not allowed when mode='automatic'")]
    UnexpectedStatus,
    #[error("unknown status '{0}' (expected 'available', 'chat', 'away', or 'dnd')")]
    UnknownStatus(String),
    #[error("unknown attribute '{0}' on <status-preference>")]
    UnknownAttribute(String),
    #[error("namespaced attribute '{0}' is not allowed on <status-preference>")]
    NamespacedAttribute(String),
    #[error("unexpected child element <{0}> in <status-preference>")]
    UnexpectedChild(String),
    #[error("unexpected text content in <status-preference>")]
    UnexpectedTextContent,
}

impl StatusPreference {
    /// The wire `mode` token (`automatic` | `manual`).
    pub fn mode_token(&self) -> &'static str {
        match self {
            StatusPreference::Automatic => MODE_AUTOMATIC,
            StatusPreference::Manual(_) => MODE_MANUAL,
        }
    }

    /// The wire `status` token, present only for a manual pick.
    pub fn status_token(&self) -> Option<&'static str> {
        match self {
            StatusPreference::Automatic => None,
            StatusPreference::Manual(status) => Some(status.as_str()),
        }
    }

    /// Build a [`StatusPreference`] from its `mode` + optional `status`
    /// wire tokens. The single source of truth for the mode×status
    /// validation rules, shared by [`parse`](Self::parse) (XML) and the
    /// wasm/JS bridge (camelCase string fields).
    pub fn from_tokens(
        mode: &str,
        status: Option<&str>,
    ) -> Result<Self, StatusPreferenceParseError> {
        match mode {
            MODE_AUTOMATIC => {
                if status.is_some() {
                    return Err(StatusPreferenceParseError::UnexpectedStatus);
                }
                Ok(StatusPreference::Automatic)
            }
            MODE_MANUAL => {
                let status = status.ok_or(StatusPreferenceParseError::MissingStatus)?;
                Ok(StatusPreference::Manual(ManualStatus::from_token(status)?))
            }
            other => Err(StatusPreferenceParseError::UnknownMode(other.to_string())),
        }
    }

    /// Serialize to a `<status-preference>` element for a PEP publish.
    pub fn build_element(&self) -> Element {
        let mut builder = Element::builder(ELEMENT_STATUS_PREFERENCE, NS_WADDLE_STATUS_PREFERENCE)
            .attr(xml_ncname!("mode").to_owned(), self.mode_token());
        if let Some(status) = self.status_token() {
            builder = builder.attr(xml_ncname!("status").to_owned(), status);
        }
        builder.build()
    }

    /// Parse a `<status-preference xmlns='urn:waddle:status-preference:0'>`
    /// element into a typed [`StatusPreference`].
    pub fn parse(element: &Element) -> Result<Self, StatusPreferenceParseError> {
        if element.name() != ELEMENT_STATUS_PREFERENCE
            || element.ns() != NS_WADDLE_STATUS_PREFERENCE
        {
            return Err(StatusPreferenceParseError::WrongRoot {
                name: element.name().to_string(),
                ns: element.ns().to_string(),
            });
        }

        reject_unknown_attrs(element, &[ATTR_MODE, ATTR_STATUS])?;
        reject_any_children(element)?;

        let mode = element
            .attr(ATTR_MODE)
            .ok_or(StatusPreferenceParseError::MissingMode)?;
        Self::from_tokens(mode, element.attr(ATTR_STATUS))
    }
}

fn reject_unknown_attrs(
    element: &Element,
    known: &[&str],
) -> Result<(), StatusPreferenceParseError> {
    for ((ns, name), _value) in element.attrs().iter() {
        let attr_name = name.as_str();
        // Prefixed attributes (non-empty namespace) are never part of
        // this XEP's contract — reject them so a client can't sneak
        // `foo:mode='bar'` past the strict-parser gate.
        if !ns.as_str().is_empty() {
            return Err(StatusPreferenceParseError::NamespacedAttribute(
                attr_name.to_string(),
            ));
        }
        if !known.contains(&attr_name) {
            return Err(StatusPreferenceParseError::UnknownAttribute(
                attr_name.to_string(),
            ));
        }
    }
    Ok(())
}

/// `<status-preference>` is a leaf element: reject both child elements
/// and any (non-whitespace) text content, which would otherwise be
/// silently persisted into `pubsub_items` as raw XML.
fn reject_any_children(element: &Element) -> Result<(), StatusPreferenceParseError> {
    if let Some(child) = element.children().next() {
        return Err(StatusPreferenceParseError::UnexpectedChild(
            child.name().to_string(),
        ));
    }
    if !element.text().trim().is_empty() {
        return Err(StatusPreferenceParseError::UnexpectedTextContent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(pref: StatusPreference) {
        let element = pref.build_element();
        let parsed = StatusPreference::parse(&element).expect("round-trip parse");
        assert_eq!(parsed, pref);
    }

    #[test]
    fn round_trip_automatic() {
        round_trip(StatusPreference::Automatic);
    }

    #[test]
    fn round_trip_manual_statuses() {
        round_trip(StatusPreference::Manual(ManualStatus::Available));
        round_trip(StatusPreference::Manual(ManualStatus::Chat));
        round_trip(StatusPreference::Manual(ManualStatus::Away));
        round_trip(StatusPreference::Manual(ManualStatus::Dnd));
    }

    fn parse_str(xml: &str) -> Result<StatusPreference, StatusPreferenceParseError> {
        let element: Element = xml.parse().expect("test fixture must be valid XML");
        StatusPreference::parse(&element)
    }

    #[test]
    fn build_automatic_omits_status() {
        let element = StatusPreference::Automatic.build_element();
        assert_eq!(element.attr("mode"), Some("automatic"));
        assert_eq!(element.attr("status"), None);
    }

    #[test]
    fn build_manual_carries_status() {
        let element = StatusPreference::Manual(ManualStatus::Dnd).build_element();
        assert_eq!(element.attr("mode"), Some("manual"));
        assert_eq!(element.attr("status"), Some("dnd"));
    }

    #[test]
    fn parse_known_manual_status_tokens() {
        assert_eq!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='manual' status='available'/>"),
            Ok(StatusPreference::Manual(ManualStatus::Available))
        );
    }

    #[test]
    fn parse_chat_status_token() {
        // RFC 6121 `chat` ("free for chat") is a pickable manual status
        // (ADR-010 Phase 5b), so it must round-trip across the user's devices.
        assert_eq!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='manual' status='chat'/>"),
            Ok(StatusPreference::Manual(ManualStatus::Chat))
        );
    }

    #[test]
    fn parse_wrong_element_rejected() {
        assert!(matches!(
            parse_str("<status xmlns='urn:waddle:status-preference:0' mode='automatic'/>"),
            Err(StatusPreferenceParseError::WrongRoot { .. })
        ));
    }

    #[test]
    fn parse_wrong_namespace_rejected() {
        assert!(matches!(
            parse_str("<status-preference xmlns='urn:example:other' mode='automatic'/>"),
            Err(StatusPreferenceParseError::WrongRoot { .. })
        ));
    }

    #[test]
    fn parse_missing_mode_rejected() {
        assert_eq!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0'/>"),
            Err(StatusPreferenceParseError::MissingMode)
        );
    }

    #[test]
    fn parse_unknown_mode_rejected() {
        assert!(matches!(
            parse_str(
                "<status-preference xmlns='urn:waddle:status-preference:0' mode='invisible'/>"
            ),
            Err(StatusPreferenceParseError::UnknownMode(_))
        ));
    }

    #[test]
    fn parse_manual_missing_status_rejected() {
        assert_eq!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='manual'/>"),
            Err(StatusPreferenceParseError::MissingStatus)
        );
    }

    #[test]
    fn parse_automatic_with_status_rejected() {
        assert_eq!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='automatic' status='away'/>"),
            Err(StatusPreferenceParseError::UnexpectedStatus)
        );
    }

    #[test]
    fn parse_unknown_status_rejected() {
        assert!(matches!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='manual' status='xa'/>"),
            Err(StatusPreferenceParseError::UnknownStatus(_))
        ));
    }

    #[test]
    fn parse_unknown_attribute_rejected() {
        assert!(matches!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='automatic' foo='bar'/>"),
            Err(StatusPreferenceParseError::UnknownAttribute(_))
        ));
    }

    #[test]
    fn parse_namespaced_attribute_rejected() {
        assert!(matches!(
            parse_str(
                "<status-preference xmlns='urn:waddle:status-preference:0' \
                 xmlns:other='urn:example:other' mode='automatic' other:mode='manual'/>"
            ),
            Err(StatusPreferenceParseError::NamespacedAttribute(_))
        ));
    }

    #[test]
    fn parse_unexpected_child_rejected() {
        assert!(matches!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='automatic'><extra/></status-preference>"),
            Err(StatusPreferenceParseError::UnexpectedChild(_))
        ));
    }

    #[test]
    fn parse_unexpected_text_content_rejected() {
        assert_eq!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='automatic'>oops</status-preference>"),
            Err(StatusPreferenceParseError::UnexpectedTextContent)
        );
    }

    #[test]
    fn from_tokens_round_trips_via_token_accessors() {
        for pref in [
            StatusPreference::Automatic,
            StatusPreference::Manual(ManualStatus::Available),
            StatusPreference::Manual(ManualStatus::Chat),
            StatusPreference::Manual(ManualStatus::Away),
            StatusPreference::Manual(ManualStatus::Dnd),
        ] {
            let rebuilt =
                StatusPreference::from_tokens(pref.mode_token(), pref.status_token()).expect("ok");
            assert_eq!(rebuilt, pref);
        }
    }

    #[test]
    fn from_tokens_enforces_mode_status_rules() {
        assert_eq!(
            StatusPreference::from_tokens("automatic", Some("away")),
            Err(StatusPreferenceParseError::UnexpectedStatus)
        );
        assert_eq!(
            StatusPreference::from_tokens("manual", None),
            Err(StatusPreferenceParseError::MissingStatus)
        );
        assert!(matches!(
            StatusPreference::from_tokens("bogus", None),
            Err(StatusPreferenceParseError::UnknownMode(_))
        ));
    }

    #[test]
    fn parse_whitespace_only_text_accepted() {
        // Pretty-printed XML carries formatting whitespace; treat it as
        // equivalent to no text.
        assert_eq!(
            parse_str("<status-preference xmlns='urn:waddle:status-preference:0' mode='automatic'>\n  </status-preference>"),
            Ok(StatusPreference::Automatic)
        );
    }
}
