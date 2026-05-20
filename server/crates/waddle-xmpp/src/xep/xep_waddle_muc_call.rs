//! `urn:waddle:muc-call:0` — Waddle MUC presence extension that
//! signals a group call is in progress in a room.
//!
//! Carried inside the occupant's `<presence/>` to the room. The
//! room broadcasts the presence to every member so all clients can
//! show a "join call" affordance. Absence of the extension means no
//! call is running.
//!
//! Wire shape:
//!
//! ```xml
//! <presence to='room@muc/A'>
//!   <call xmlns='urn:waddle:muc-call:0' state='active' call-id='r1'/>
//! </presence>
//! ```
//!
//! The two states currently defined are `active` (call is open and
//! joinable) and `inactive` (call has ended; sent by the focus when
//! the last participant leaves so clients can clear their UI). A
//! custom namespace is justified because no XEP models SFU-backed
//! group-call state.

use minidom::Element;
use thiserror::Error;

use waddle_sfu::{CallId, SfuError};

pub const NS_WADDLE_MUC_CALL: &str = "urn:waddle:muc-call:0";
const CALL_NAME: &str = "call";
const ATTR_STATE: &str = "state";
const ATTR_CALL_ID: &str = "call-id";
const STATE_ACTIVE: &str = "active";
const STATE_INACTIVE: &str = "inactive";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPresenceState {
    Active,
    Inactive,
}

impl CallPresenceState {
    fn as_attr(self) -> &'static str {
        match self {
            Self::Active => STATE_ACTIVE,
            Self::Inactive => STATE_INACTIVE,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            STATE_ACTIVE => Some(Self::Active),
            STATE_INACTIVE => Some(Self::Inactive),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MucCallExtension {
    pub state: CallPresenceState,
    pub call_id: CallId,
}

impl MucCallExtension {
    pub fn active(call_id: CallId) -> Self {
        Self {
            state: CallPresenceState::Active,
            call_id,
        }
    }

    pub fn inactive(call_id: CallId) -> Self {
        Self {
            state: CallPresenceState::Inactive,
            call_id,
        }
    }

    pub fn to_element(&self) -> Element {
        Element::builder(CALL_NAME, NS_WADDLE_MUC_CALL)
            .attr(
                minidom::rxml::xml_ncname!("state").to_owned(),
                self.state.as_attr(),
            )
            .attr(
                minidom::rxml::xml_ncname!("call-id").to_owned(),
                self.call_id.as_str(),
            )
            .build()
    }
}

#[derive(Debug, Error)]
pub enum MucCallParseError {
    #[error("element is not a <call/> in urn:waddle:muc-call:0")]
    WrongElement,
    #[error("missing required attribute: {0}")]
    MissingAttribute(&'static str),
    #[error("invalid state value: {0}")]
    InvalidState(String),
    #[error(transparent)]
    InvalidCallId(SfuError),
}

impl TryFrom<&Element> for MucCallExtension {
    type Error = MucCallParseError;

    fn try_from(elem: &Element) -> Result<Self, Self::Error> {
        if elem.name() != CALL_NAME || elem.ns() != NS_WADDLE_MUC_CALL {
            return Err(MucCallParseError::WrongElement);
        }
        let state = elem
            .attr(ATTR_STATE)
            .ok_or(MucCallParseError::MissingAttribute(ATTR_STATE))?;
        let call_id_raw = elem
            .attr(ATTR_CALL_ID)
            .ok_or(MucCallParseError::MissingAttribute(ATTR_CALL_ID))?;

        let state = CallPresenceState::parse(state)
            .ok_or_else(|| MucCallParseError::InvalidState(state.to_string()))?;
        let call_id = CallId::new(call_id_raw).map_err(MucCallParseError::InvalidCallId)?;
        Ok(Self { state, call_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_round_trips() {
        let ext = MucCallExtension::active(CallId::new("r1").unwrap());
        let elem = ext.to_element();
        assert_eq!(elem.attr(ATTR_STATE), Some("active"));
        assert_eq!(elem.attr(ATTR_CALL_ID), Some("r1"));

        let reparsed = MucCallExtension::try_from(&elem).unwrap();
        assert_eq!(reparsed.state, CallPresenceState::Active);
        assert_eq!(reparsed.call_id.as_str(), "r1");
    }

    #[test]
    fn inactive_round_trips() {
        let ext = MucCallExtension::inactive(CallId::new("r1").unwrap());
        let elem = ext.to_element();
        assert_eq!(elem.attr(ATTR_STATE), Some("inactive"));

        let reparsed = MucCallExtension::try_from(&elem).unwrap();
        assert_eq!(reparsed.state, CallPresenceState::Inactive);
    }

    #[test]
    fn rejects_unknown_state() {
        let xml = "<call xmlns='urn:waddle:muc-call:0' state='ringing' call-id='r1'/>";
        let elem: Element = xml.parse().unwrap();
        let err = MucCallExtension::try_from(&elem).unwrap_err();
        assert!(matches!(err, MucCallParseError::InvalidState(_)));
    }

    #[test]
    fn rejects_missing_attributes() {
        let xml = "<call xmlns='urn:waddle:muc-call:0' state='active'/>";
        let elem: Element = xml.parse().unwrap();
        let err = MucCallExtension::try_from(&elem).unwrap_err();
        match err {
            MucCallParseError::MissingAttribute(attr) => assert_eq!(attr, ATTR_CALL_ID),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_namespace() {
        let xml = "<call xmlns='urn:waddle:muc-call:1' state='active' call-id='r1'/>";
        let elem: Element = xml.parse().unwrap();
        let err = MucCallExtension::try_from(&elem).unwrap_err();
        assert!(matches!(err, MucCallParseError::WrongElement));
    }
}
