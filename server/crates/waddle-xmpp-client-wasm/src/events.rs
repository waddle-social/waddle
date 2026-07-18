use super::*;

pub(crate) async fn event_dispatch_loop(
    inner: Rc<RefCell<WaddleClientInner>>,
    mut event_rx: mpsc::Receiver<DriverEvent>,
) {
    while let Some(event) = event_rx.next().await {
        match event {
            DriverEvent::Client(client_event) => dispatch_client_event(&inner, *client_event),
            DriverEvent::ResumeState(state) => {
                inner.borrow_mut().resume_state = state;
            }
            DriverEvent::Error {
                reason,
                authentication_condition,
            } => emit_error_callback(&inner, reason, authentication_condition),
            DriverEvent::Disconnected => {
                // Clone the callback before invoking it so a JS handler that
                // synchronously re-enters the WaddleClient via a `send_*`
                // method (which takes `borrow_mut`) does not panic on an
                // outstanding `Rc<RefCell>` borrow held across the call. The
                // same pattern is applied to every other `on_*` dispatch site
                // below.
                let callback = inner.borrow().on_disconnected.clone();
                inner.borrow_mut().retire();
                if let Some(callback) = callback {
                    let _ = callback.call0(&JsValue::NULL);
                }
            }
        }
    }
}

pub(crate) fn dispatch_client_event(inner: &Rc<RefCell<WaddleClientInner>>, event: ClientEvent) {
    if inner.borrow().disposed {
        return;
    }
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => {
            // Snapshot both callbacks BEFORE invoking either; a JS handler
            // that re-enters `WaddleClient::set_on_*` would otherwise drop
            // the live borrow mid-iteration.
            let (on_connected, on_session_lifecycle) = {
                let borrowed = inner.borrow();
                (
                    borrowed.on_connected.clone(),
                    borrowed.on_session_lifecycle.clone(),
                )
            };
            if let Some(callback) = on_connected {
                let _ = callback.call0(&JsValue::NULL);
            }
            if let Some(callback) = on_session_lifecycle {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str("fresh"));
            }
        }
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Resumed { .. },
        )) => {
            let callback = inner.borrow().on_session_lifecycle.clone();
            if let Some(callback) = callback {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str("resumed"));
            }
        }
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckRequestSent { attempt, unacked },
        )) => emit_stream_management_callback(
            inner,
            JsStreamManagementTelemetry::Request { attempt, unacked },
        ),
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckObserved {
                progressed,
                latency_ms,
                unacked,
            },
        )) => emit_stream_management_callback(
            inner,
            JsStreamManagementTelemetry::Observed {
                progressed,
                latency_ms,
                unacked,
            },
        ),
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckRequestTimedOut { unacked },
        )) => emit_stream_management_callback(
            inner,
            JsStreamManagementTelemetry::RequestTimeout { unacked },
        ),
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckProgressStalled {
                unacked,
                elapsed_ms,
            },
        )) => emit_stream_management_callback(
            inner,
            JsStreamManagementTelemetry::ProgressStalled {
                unacked,
                elapsed_ms,
            },
        ),
        ClientEvent::Connection(ConnectionEvent::StreamError { condition, detail }) => {
            emit_stream_error_callback(inner, condition, detail);
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Message(message)) => {
            let mut message = *message;
            let account_bare_jid = {
                let borrowed = inner.borrow();
                bare_jid(&borrowed.config.jid)
            };
            if message.mds_displayed.is_some()
                && !waddle_xmpp_client::mds::mds_event_from_matches_account(
                    message.from.as_deref(),
                    &account_bare_jid,
                )
            {
                message.mds_displayed = None;
            }
            // XEP-0490 §3.2: surface MDS PEP events through the
            // dedicated `on_mds_displayed` callback as well as the
            // generic on_message stream. Either path is safe; the
            // chat layer applies state from `on_mds_displayed` and
            // drops the message-shaped echo for MDS events.
            if let Some(entries) = message.mds_displayed.as_ref() {
                let callback = inner.borrow().on_mds_displayed.clone();
                if let Some(callback) = callback {
                    for entry in entries {
                        let js_entry = WaddleMdsDisplayedEntry {
                            chat_id: entry.chat_id.to_string(),
                            stanza_id: entry.stanza_id.as_str().to_string(),
                            stanza_id_by: entry.stanza_id_by.to_string(),
                        };
                        if let Ok(value) = to_js_value(&js_entry) {
                            let _ = callback.call1(&JsValue::NULL, &value);
                        }
                    }
                }
            }
            if !message.pubsub_events.is_empty() {
                let callback = inner.borrow().on_pubsub_event.clone();
                if let Some(callback) = callback {
                    for event in message.pubsub_events.iter().cloned() {
                        if let Ok(value) = to_js_value(&pubsub_event_to_js(event)) {
                            let _ = callback.call1(&JsValue::NULL, &value);
                        }
                    }
                }
            }
            let callback = inner.borrow().on_message.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&inbound_to_js(message)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Presence(presence)) => {
            let callback = inner.borrow().on_presence.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&presence_to_js(*presence)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::InboxStreamEntry(entry) => {
            let callback = inner.borrow().on_message.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&inbox_push_to_js(entry)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id }) => {
            let callback = inner.borrow().on_message_delivery_acked.clone();
            if let Some(callback) = callback {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(stanza_id.as_str()));
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id }) => {
            let callback = inner.borrow().on_message_delivery_failed.clone();
            if let Some(callback) = callback {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(stanza_id.as_str()));
            }
        }
        ClientEvent::Call(call_event) => {
            let callback = inner.borrow().on_call.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&call_event_to_js(*call_event)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        _ => {}
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum JsStreamManagementTelemetry {
    #[serde(rename = "ack-request")]
    Request { attempt: u32, unacked: u32 },
    #[serde(rename = "ack-observed")]
    Observed {
        progressed: bool,
        latency_ms: Option<u64>,
        unacked: u32,
    },
    #[serde(rename = "ack-request-timeout")]
    RequestTimeout { unacked: u32 },
    #[serde(rename = "ack-progress-stalled")]
    ProgressStalled { unacked: u32, elapsed_ms: u64 },
}

fn emit_stream_management_callback(
    inner: &Rc<RefCell<WaddleClientInner>>,
    event: JsStreamManagementTelemetry,
) {
    let callback = inner.borrow().on_stream_management.clone();
    if let Some(callback) = callback {
        if let Ok(value) = to_js_value(&event) {
            let _ = callback.call1(&JsValue::NULL, &value);
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum JsControlErrorKind {
    DriverError,
    StreamError,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsDriverError {
    kind: JsControlErrorKind,
    reason: DriverErrorReason,
    authentication_condition: Option<DriverAuthenticationCondition>,
}

pub(crate) fn emit_error_callback(
    inner: &Rc<RefCell<WaddleClientInner>>,
    reason: DriverErrorReason,
    authentication_condition: Option<DriverAuthenticationCondition>,
) {
    let callback = inner.borrow().on_error.clone();
    if let Some(callback) = callback {
        let payload = JsDriverError {
            kind: JsControlErrorKind::DriverError,
            reason,
            authentication_condition,
        };
        if let Ok(value) = to_js_value(&payload) {
            let _ = callback.call1(&JsValue::NULL, &value);
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum JsStreamErrorCondition {
    BadFormat,
    BadNamespacePrefix,
    Conflict,
    ConnectionTimeout,
    HostGone,
    HostUnknown,
    ImproperAddressing,
    InternalServerError,
    InvalidFrom,
    InvalidNamespace,
    InvalidXml,
    NotAuthorized,
    NotWellFormed,
    PolicyViolation,
    RemoteConnectionFailed,
    Reset,
    ResourceConstraint,
    RestrictedXml,
    SeeOtherHost,
    SystemShutdown,
    UndefinedCondition,
    UnsupportedEncoding,
    UnsupportedFeature,
    UnsupportedStanzaType,
    UnsupportedVersion,
}

impl From<StreamErrorCondition> for JsStreamErrorCondition {
    fn from(value: StreamErrorCondition) -> Self {
        match value {
            StreamErrorCondition::BadFormat => Self::BadFormat,
            StreamErrorCondition::BadNamespacePrefix => Self::BadNamespacePrefix,
            StreamErrorCondition::Conflict => Self::Conflict,
            StreamErrorCondition::ConnectionTimeout => Self::ConnectionTimeout,
            StreamErrorCondition::HostGone => Self::HostGone,
            StreamErrorCondition::HostUnknown => Self::HostUnknown,
            StreamErrorCondition::ImproperAddressing => Self::ImproperAddressing,
            StreamErrorCondition::InternalServerError => Self::InternalServerError,
            StreamErrorCondition::InvalidFrom => Self::InvalidFrom,
            StreamErrorCondition::InvalidNamespace => Self::InvalidNamespace,
            StreamErrorCondition::InvalidXml => Self::InvalidXml,
            StreamErrorCondition::NotAuthorized => Self::NotAuthorized,
            StreamErrorCondition::NotWellFormed => Self::NotWellFormed,
            StreamErrorCondition::PolicyViolation => Self::PolicyViolation,
            StreamErrorCondition::RemoteConnectionFailed => Self::RemoteConnectionFailed,
            StreamErrorCondition::Reset => Self::Reset,
            StreamErrorCondition::ResourceConstraint => Self::ResourceConstraint,
            StreamErrorCondition::RestrictedXml => Self::RestrictedXml,
            StreamErrorCondition::SeeOtherHost => Self::SeeOtherHost,
            StreamErrorCondition::SystemShutdown => Self::SystemShutdown,
            StreamErrorCondition::UndefinedCondition => Self::UndefinedCondition,
            StreamErrorCondition::UnsupportedEncoding => Self::UnsupportedEncoding,
            StreamErrorCondition::UnsupportedFeature => Self::UnsupportedFeature,
            StreamErrorCondition::UnsupportedStanzaType => Self::UnsupportedStanzaType,
            StreamErrorCondition::UnsupportedVersion => Self::UnsupportedVersion,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsStreamError {
    kind: JsControlErrorKind,
    condition: JsStreamErrorCondition,
    stream_management_error: Option<JsStreamManagementError>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum JsStreamManagementError {
    #[serde(rename = "handled-count-too-high")]
    HandledCountTooHigh {
        h: u32,
        #[serde(rename = "sendCount")]
        send_count: u32,
    },
}

fn emit_stream_error_callback(
    inner: &Rc<RefCell<WaddleClientInner>>,
    condition: StreamErrorCondition,
    detail: Option<StreamErrorDetail>,
) {
    let callback = inner.borrow().on_error.clone();
    if let Some(callback) = callback {
        let stream_management_error = stream_management_error_to_js(detail);
        let payload = JsStreamError {
            kind: JsControlErrorKind::StreamError,
            condition: condition.into(),
            stream_management_error,
        };
        if let Ok(value) = to_js_value(&payload) {
            let _ = callback.call1(&JsValue::NULL, &value);
        }
    }
}

fn stream_management_error_to_js(
    detail: Option<StreamErrorDetail>,
) -> Option<JsStreamManagementError> {
    detail.map(|StreamErrorDetail::HandledCountTooHigh { h, send_count }| {
        JsStreamManagementError::HandledCountTooHigh { h, send_count }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn driver_error_reason_codes_cover_every_typed_variant() {
        assert_eq!(
            DriverErrorReason::ALL
                .iter()
                .copied()
                .map(|reason| serde_json::to_value(reason).expect("reason serializes"))
                .collect::<Vec<_>>(),
            [
                "core-error",
                "invalid-transport-scheme",
                "missing-websocket-host",
                "empty-resource",
                "empty-stanza-id",
                "request-id-exhausted",
                "duplicate-request",
                "duplicate-stanza-correlation",
                "unknown-request",
                "unknown-stanza-correlation",
                "invalid-phase-transition",
                "invalid-state-transition",
                "missing-stream-feature",
                "invalid-stream-features",
                "invalid-sasl-failure",
                "invalid-bind-response",
                "authentication-rejected",
                "websocket-connect-timeout",
                "websocket-write-timeout",
                "iq-timeout",
                "websocket-transport-error",
                "empty-transport-frame",
                "transport-frame-too-large",
                "invalid-transport-frame",
                "invalid-stream-open-to",
                "invalid-stream-open-from",
                "unsupported-stream-version",
                "unsupported-websocket-message",
                "transport-closed",
                "request-cancelled",
                "disconnected",
                "invalid-resume-stanza",
                "push-registration-error",
                "stanza-error",
            ]
            .map(serde_json::Value::from),
        );
    }

    #[test]
    fn driver_authentication_codes_cover_every_typed_variant() {
        assert_eq!(
            DriverAuthenticationCondition::ALL
                .iter()
                .copied()
                .map(|condition| {
                    serde_json::to_value(condition).expect("authentication condition serializes")
                })
                .collect::<Vec<_>>(),
            [
                "aborted",
                "account-disabled",
                "credentials-expired",
                "encryption-required",
                "incorrect-encoding",
                "invalid-authzid",
                "invalid-mechanism",
                "malformed-request",
                "mechanism-too-weak",
                "not-authorized",
                "temporary-auth-failure",
                "unknown",
            ]
            .map(serde_json::Value::from),
        );
    }

    #[test]
    fn stream_error_codes_cover_every_core_typed_variant() {
        assert_eq!(
            StreamErrorCondition::ALL
                .into_iter()
                .map(JsStreamErrorCondition::from)
                .map(|condition| {
                    serde_json::to_value(condition).expect("stream condition serializes")
                })
                .collect::<Vec<_>>(),
            [
                "bad-format",
                "bad-namespace-prefix",
                "conflict",
                "connection-timeout",
                "host-gone",
                "host-unknown",
                "improper-addressing",
                "internal-server-error",
                "invalid-from",
                "invalid-namespace",
                "invalid-xml",
                "not-authorized",
                "not-well-formed",
                "policy-violation",
                "remote-connection-failed",
                "reset",
                "resource-constraint",
                "restricted-xml",
                "see-other-host",
                "system-shutdown",
                "undefined-condition",
                "unsupported-encoding",
                "unsupported-feature",
                "unsupported-stanza-type",
                "unsupported-version",
            ]
            .map(serde_json::Value::from),
        );
    }

    #[test]
    fn driver_errors_serialize_as_tagged_payloads_with_typed_authentication_condition() {
        let payload = JsDriverError {
            kind: JsControlErrorKind::DriverError,
            reason: DriverErrorReason::AuthenticationRejected,
            authentication_condition: Some(DriverAuthenticationCondition::NotAuthorized),
        };

        assert_eq!(
            serde_json::to_value(payload).expect("driver error serializes"),
            json!({
                "kind": "driver-error",
                "reason": "authentication-rejected",
                "authenticationCondition": "not-authorized",
            }),
        );
    }

    #[test]
    fn websocket_transport_errors_serialize_without_source_detail() {
        let payload = JsDriverError {
            kind: JsControlErrorKind::DriverError,
            reason: DriverErrorReason::WebSocketTransport,
            authentication_condition: None,
        };

        assert_eq!(
            serde_json::to_value(payload).expect("driver error serializes"),
            json!({
                "kind": "driver-error",
                "reason": "websocket-transport-error",
                "authenticationCondition": null,
            }),
        );
    }

    #[test]
    fn stream_errors_serialize_as_tagged_payloads_with_typed_sm_metadata() {
        let payload = JsStreamError {
            kind: JsControlErrorKind::StreamError,
            condition: JsStreamErrorCondition::from(StreamErrorCondition::UndefinedCondition),
            stream_management_error: Some(JsStreamManagementError::HandledCountTooHigh {
                h: 3,
                send_count: 2,
            }),
        };

        assert_eq!(
            serde_json::to_value(payload).expect("stream error serializes"),
            json!({
                "kind": "stream-error",
                "condition": "undefined-condition",
                "streamManagementError": {
                    "kind": "handled-count-too-high",
                    "h": 3,
                    "sendCount": 2,
                },
            }),
        );
    }
}
