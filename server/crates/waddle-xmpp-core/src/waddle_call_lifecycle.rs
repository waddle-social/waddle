//! A client's closed-set call lifecycle report to its own server.
//!
//! `accepted` means the call's media session connected on the reporting
//! device. It does not describe a signalling acknowledgement from a peer.
//!
//! Reports use an IQ set carrying `<report/>` in
//! `urn:waddle:call-lifecycle:0`. All six attributes are required; unknown
//! extra attributes and child elements are ignored for forward compatibility.
//! The server only counts and acknowledges a report: it is stateless and
//! does not deduplicate. A client sends each report at most once and never
//! retries. There is no timestamp on the wire.

use minidom::rxml::xml_ncname;
use minidom::Element;
use thiserror::Error;

/// Namespace of the call lifecycle report payload.
pub const NS_WADDLE_CALL_LIFECYCLE: &str = "urn:waddle:call-lifecycle:0";

/// The call topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallLifecycleKind {
    /// A direct call.
    Dm,
    /// A multi-user room call.
    Muc,
}

impl CallLifecycleKind {
    /// Every supported value.
    pub const ALL: [Self; 2] = [Self::Dm, Self::Muc];

    /// The canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dm => "dm",
            Self::Muc => "muc",
        }
    }

    /// Parse a canonical wire token.
    ///
    /// # Errors
    /// Returns [`CallLifecycleParseError::UnknownKind`] for an unknown value.
    pub fn from_token(raw: &str) -> Result<Self, CallLifecycleParseError> {
        match raw {
            "dm" => Ok(Self::Dm),
            "muc" => Ok(Self::Muc),
            _ => Err(CallLifecycleParseError::UnknownKind),
        }
    }
}

/// The reporting device's setup outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSetupOutcome {
    /// The call was proposed.
    Proposed,
    /// The media session connected on the reporting device.
    Accepted,
    /// The call was declined.
    Declined,
    /// Setup timed out.
    Timeout,
    /// Setup failed.
    Failed,
}

impl CallSetupOutcome {
    /// Every supported value.
    pub const ALL: [Self; 5] = [
        Self::Proposed,
        Self::Accepted,
        Self::Declined,
        Self::Timeout,
        Self::Failed,
    ];

    /// The canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Timeout => "timeout",
            Self::Failed => "failed",
        }
    }

    /// Parse a canonical wire token.
    ///
    /// # Errors
    /// Returns [`CallLifecycleParseError::UnknownSetupOutcome`] for an unknown value.
    pub fn from_token(raw: &str) -> Result<Self, CallLifecycleParseError> {
        match raw {
            "proposed" => Ok(Self::Proposed),
            "accepted" => Ok(Self::Accepted),
            "declined" => Ok(Self::Declined),
            "timeout" => Ok(Self::Timeout),
            "failed" => Ok(Self::Failed),
            _ => Err(CallLifecycleParseError::UnknownSetupOutcome),
        }
    }
}

/// The reporting device's reason for ending the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEndReason {
    /// A participant hung up.
    Hangup,
    /// The peer left.
    PeerLeft,
    /// The call ended with an error.
    Error,
    /// Reconnect attempts were exhausted.
    ReconnectExhausted,
}

impl CallEndReason {
    /// Every supported value.
    pub const ALL: [Self; 4] = [
        Self::Hangup,
        Self::PeerLeft,
        Self::Error,
        Self::ReconnectExhausted,
    ];

    /// The canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hangup => "hangup",
            Self::PeerLeft => "peer-left",
            Self::Error => "error",
            Self::ReconnectExhausted => "reconnect-exhausted",
        }
    }

    /// Parse a canonical wire token.
    ///
    /// # Errors
    /// Returns [`CallLifecycleParseError::UnknownEndReason`] for an unknown value.
    pub fn from_token(raw: &str) -> Result<Self, CallLifecycleParseError> {
        match raw {
            "hangup" => Ok(Self::Hangup),
            "peer-left" => Ok(Self::PeerLeft),
            "error" => Ok(Self::Error),
            "reconnect-exhausted" => Ok(Self::ReconnectExhausted),
            _ => Err(CallLifecycleParseError::UnknownEndReason),
        }
    }
}

/// The closed duration bucket reported by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDurationBucket {
    /// No connected duration.
    None,
    /// Less than one minute.
    Under1m,
    /// One to ten minutes.
    OneToTenMinutes,
    /// Ten to sixty minutes.
    TenToSixtyMinutes,
    /// More than sixty minutes.
    Over60m,
}

impl CallDurationBucket {
    /// Every supported value.
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Under1m,
        Self::OneToTenMinutes,
        Self::TenToSixtyMinutes,
        Self::Over60m,
    ];

    /// The canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Under1m => "under-1m",
            Self::OneToTenMinutes => "1m-10m",
            Self::TenToSixtyMinutes => "10m-60m",
            Self::Over60m => "over-60m",
        }
    }

    /// Parse a canonical wire token.
    ///
    /// # Errors
    /// Returns [`CallLifecycleParseError::UnknownDuration`] for an unknown value.
    pub fn from_token(raw: &str) -> Result<Self, CallLifecycleParseError> {
        match raw {
            "none" => Ok(Self::None),
            "under-1m" => Ok(Self::Under1m),
            "1m-10m" => Ok(Self::OneToTenMinutes),
            "10m-60m" => Ok(Self::TenToSixtyMinutes),
            "over-60m" => Ok(Self::Over60m),
            _ => Err(CallLifecycleParseError::UnknownDuration),
        }
    }
}

/// The closed reconnect-count bucket reported by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallReconnectBucket {
    /// No reconnects.
    None,
    /// One reconnect.
    Once,
    /// More than one reconnect.
    Multiple,
}

impl CallReconnectBucket {
    /// Every supported value.
    pub const ALL: [Self; 3] = [Self::None, Self::Once, Self::Multiple];

    /// The canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Once => "once",
            Self::Multiple => "multiple",
        }
    }

    /// Parse a canonical wire token.
    ///
    /// # Errors
    /// Returns [`CallLifecycleParseError::UnknownReconnects`] for an unknown value.
    pub fn from_token(raw: &str) -> Result<Self, CallLifecycleParseError> {
        match raw {
            "none" => Ok(Self::None),
            "once" => Ok(Self::Once),
            "multiple" => Ok(Self::Multiple),
            _ => Err(CallLifecycleParseError::UnknownReconnects),
        }
    }
}

/// The reporting client's platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallClientPlatform {
    /// An iOS client.
    Ios,
    /// A macOS client.
    Macos,
    /// An Android client.
    Android,
    /// A web client.
    Web,
}

impl CallClientPlatform {
    /// Every supported value.
    pub const ALL: [Self; 4] = [Self::Ios, Self::Macos, Self::Android, Self::Web];

    /// The canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Macos => "macos",
            Self::Android => "android",
            Self::Web => "web",
        }
    }

    /// Parse a canonical wire token.
    ///
    /// # Errors
    /// Returns [`CallLifecycleParseError::UnknownPlatform`] for an unknown value.
    pub fn from_token(raw: &str) -> Result<Self, CallLifecycleParseError> {
        match raw {
            "ios" => Ok(Self::Ios),
            "macos" => Ok(Self::Macos),
            "android" => Ok(Self::Android),
            "web" => Ok(Self::Web),
            _ => Err(CallLifecycleParseError::UnknownPlatform),
        }
    }
}

/// A validated client report. Each field is a closed wire value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallLifecycleReport {
    /// The client's `kind` value.
    pub kind: CallLifecycleKind,
    /// The client's `setup-outcome` value.
    pub setup_outcome: CallSetupOutcome,
    /// The client's `end-reason` value.
    pub end_reason: CallEndReason,
    /// The client's `duration` value.
    pub duration: CallDurationBucket,
    /// The client's `reconnects` value.
    pub reconnects: CallReconnectBucket,
    /// The client's `platform` value.
    pub platform: CallClientPlatform,
}

/// Invalid call lifecycle report payloads.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CallLifecycleParseError {
    /// The root name or namespace is incorrect.
    #[error("expected a report in urn:waddle:call-lifecycle:0")]
    WrongRoot,
    /// The required `kind` attribute is absent.
    #[error("report requires the kind attribute")]
    MissingKind,
    /// The `kind` attribute has an unknown wire value.
    #[error("unknown kind value")]
    UnknownKind,
    /// The required `setup-outcome` attribute is absent.
    #[error("report requires the setup-outcome attribute")]
    MissingSetupOutcome,
    /// The `setup-outcome` attribute has an unknown wire value.
    #[error("unknown setup-outcome value")]
    UnknownSetupOutcome,
    /// The required `end-reason` attribute is absent.
    #[error("report requires the end-reason attribute")]
    MissingEndReason,
    /// The `end-reason` attribute has an unknown wire value.
    #[error("unknown end-reason value")]
    UnknownEndReason,
    /// The required `duration` attribute is absent.
    #[error("report requires the duration attribute")]
    MissingDuration,
    /// The `duration` attribute has an unknown wire value.
    #[error("unknown duration value")]
    UnknownDuration,
    /// The required `reconnects` attribute is absent.
    #[error("report requires the reconnects attribute")]
    MissingReconnects,
    /// The `reconnects` attribute has an unknown wire value.
    #[error("unknown reconnects value")]
    UnknownReconnects,
    /// The required `platform` attribute is absent.
    #[error("report requires the platform attribute")]
    MissingPlatform,
    /// The `platform` attribute has an unknown wire value.
    #[error("unknown platform value")]
    UnknownPlatform,
}

impl CallLifecycleReport {
    /// Build the report payload for an IQ set.
    #[must_use]
    pub fn build_element(&self) -> Element {
        Element::builder("report", NS_WADDLE_CALL_LIFECYCLE)
            .attr(xml_ncname!("kind").to_owned(), self.kind.as_str())
            .attr(
                xml_ncname!("setup-outcome").to_owned(),
                self.setup_outcome.as_str(),
            )
            .attr(
                xml_ncname!("end-reason").to_owned(),
                self.end_reason.as_str(),
            )
            .attr(xml_ncname!("duration").to_owned(), self.duration.as_str())
            .attr(
                xml_ncname!("reconnects").to_owned(),
                self.reconnects.as_str(),
            )
            .attr(xml_ncname!("platform").to_owned(), self.platform.as_str())
            .build()
    }
}

impl TryFrom<&Element> for CallLifecycleReport {
    type Error = CallLifecycleParseError;

    fn try_from(element: &Element) -> Result<Self, Self::Error> {
        if element.name() != "report" || element.ns() != NS_WADDLE_CALL_LIFECYCLE {
            return Err(CallLifecycleParseError::WrongRoot);
        }
        Ok(Self {
            kind: CallLifecycleKind::from_token(
                element
                    .attr("kind")
                    .ok_or(CallLifecycleParseError::MissingKind)?,
            )?,
            setup_outcome: CallSetupOutcome::from_token(
                element
                    .attr("setup-outcome")
                    .ok_or(CallLifecycleParseError::MissingSetupOutcome)?,
            )?,
            end_reason: CallEndReason::from_token(
                element
                    .attr("end-reason")
                    .ok_or(CallLifecycleParseError::MissingEndReason)?,
            )?,
            duration: CallDurationBucket::from_token(
                element
                    .attr("duration")
                    .ok_or(CallLifecycleParseError::MissingDuration)?,
            )?,
            reconnects: CallReconnectBucket::from_token(
                element
                    .attr("reconnects")
                    .ok_or(CallLifecycleParseError::MissingReconnects)?,
            )?,
            platform: CallClientPlatform::from_token(
                element
                    .attr("platform")
                    .ok_or(CallLifecycleParseError::MissingPlatform)?,
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> CallLifecycleReport {
        CallLifecycleReport {
            kind: CallLifecycleKind::Dm,
            setup_outcome: CallSetupOutcome::Accepted,
            end_reason: CallEndReason::Hangup,
            duration: CallDurationBucket::None,
            reconnects: CallReconnectBucket::None,
            platform: CallClientPlatform::Ios,
        }
    }

    fn round_trip(report: CallLifecycleReport) {
        let element = report.build_element();
        assert_eq!(element.name(), "report");
        assert_eq!(element.ns(), NS_WADDLE_CALL_LIFECYCLE);
        assert_eq!(element.attrs().len(), 6);
        assert_eq!(element.children().count(), 0);
        assert_eq!(CallLifecycleReport::try_from(&element), Ok(report));
    }

    #[test]
    fn round_trip_every_closed_value() {
        for value in CallLifecycleKind::ALL {
            assert_eq!(CallLifecycleKind::from_token(value.as_str()), Ok(value));
            round_trip(CallLifecycleReport {
                kind: value,
                ..report()
            });
        }
        for value in CallSetupOutcome::ALL {
            assert_eq!(CallSetupOutcome::from_token(value.as_str()), Ok(value));
            round_trip(CallLifecycleReport {
                setup_outcome: value,
                ..report()
            });
        }
        for value in CallEndReason::ALL {
            assert_eq!(CallEndReason::from_token(value.as_str()), Ok(value));
            round_trip(CallLifecycleReport {
                end_reason: value,
                ..report()
            });
        }
        for value in CallDurationBucket::ALL {
            assert_eq!(CallDurationBucket::from_token(value.as_str()), Ok(value));
            round_trip(CallLifecycleReport {
                duration: value,
                ..report()
            });
        }
        for value in CallReconnectBucket::ALL {
            assert_eq!(CallReconnectBucket::from_token(value.as_str()), Ok(value));
            round_trip(CallLifecycleReport {
                reconnects: value,
                ..report()
            });
        }
        for value in CallClientPlatform::ALL {
            assert_eq!(CallClientPlatform::from_token(value.as_str()), Ok(value));
            round_trip(CallLifecycleReport {
                platform: value,
                ..report()
            });
        }
    }

    fn with_attributes(omit: Option<&str>, unknown: Option<&str>) -> Element {
        let fixture = report().build_element();
        let mut builder = Element::builder("report", NS_WADDLE_CALL_LIFECYCLE);
        for ((_, name), value) in fixture.attrs().iter() {
            if Some(name.as_str()) == omit {
                continue;
            }
            let value = if Some(name.as_str()) == unknown {
                "unknown"
            } else {
                value
            };
            builder = builder.attr(name.clone(), value);
        }
        builder.build()
    }

    #[test]
    fn each_missing_attribute_is_rejected() {
        for (attribute, error) in [
            ("kind", CallLifecycleParseError::MissingKind),
            (
                "setup-outcome",
                CallLifecycleParseError::MissingSetupOutcome,
            ),
            ("end-reason", CallLifecycleParseError::MissingEndReason),
            ("duration", CallLifecycleParseError::MissingDuration),
            ("reconnects", CallLifecycleParseError::MissingReconnects),
            ("platform", CallLifecycleParseError::MissingPlatform),
        ] {
            assert_eq!(
                CallLifecycleReport::try_from(&with_attributes(Some(attribute), None)),
                Err(error)
            );
        }
    }

    #[test]
    fn each_unknown_attribute_value_is_rejected() {
        for (attribute, error) in [
            ("kind", CallLifecycleParseError::UnknownKind),
            (
                "setup-outcome",
                CallLifecycleParseError::UnknownSetupOutcome,
            ),
            ("end-reason", CallLifecycleParseError::UnknownEndReason),
            ("duration", CallLifecycleParseError::UnknownDuration),
            ("reconnects", CallLifecycleParseError::UnknownReconnects),
            ("platform", CallLifecycleParseError::UnknownPlatform),
        ] {
            assert_eq!(
                CallLifecycleReport::try_from(&with_attributes(None, Some(attribute))),
                Err(error)
            );
        }
    }

    #[test]
    fn unknown_extra_attribute_and_child_are_ignored() {
        let mut element = report().build_element();
        element.set_attr(
            minidom::rxml::Namespace::NONE,
            xml_ncname!("future").to_owned(),
            "unknown",
        );
        element.append_child(Element::builder("future", "urn:waddle:future:0").build());
        assert_eq!(CallLifecycleReport::try_from(&element), Ok(report()));
    }

    #[test]
    fn wrong_namespace_or_name_is_rejected() {
        for element in [
            Element::builder("other", NS_WADDLE_CALL_LIFECYCLE).build(),
            Element::builder("report", "urn:waddle:other:0").build(),
        ] {
            assert_eq!(
                CallLifecycleReport::try_from(&element),
                Err(CallLifecycleParseError::WrongRoot)
            );
        }
    }
}
