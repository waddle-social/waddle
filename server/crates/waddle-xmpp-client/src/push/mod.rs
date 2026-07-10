//! Push device registration — XEP-0050 multi-step composer + typed
//! value objects.
//!
//! The chat / mobile clients never build the wire `<command/>`
//! directly. They call [`register_push_device`], which drives the
//! four-stage XEP-0050 §3 handshake against the Push Service:
//!
//! 1. `<command node='register-device' action='execute'/>` →
//!    Push Service.
//! 2. Push Service responds `status='executing'` + form prompt +
//!    `sessionid`.
//! 3. Client submits the platform-discriminated XEP-0004 form back
//!    with `action='complete'` carrying the `sessionid`.
//! 4. Push Service responds `status='completed'` + result form whose
//!    `node` field carries the assigned XEP-0357 node id.
//!
//! Storage layout (the `push_nodes` / `push_devices` tables in
//! `waddle-server`) is unchanged by the XEP-0050 cutover — only the
//! wire shape changes.

use std::str::FromStr;

use jid::BareJid;
use minidom::Element;
use thiserror::Error;
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};

use crate::error::{ClientError, ClientResult, StanzaError};
use crate::xep::xep0050::{
    build_xep0050_command_request, build_xep0050_command_request_with_session,
    parse_command_response, AdHocAction, AdHocStatus,
};

/// XEP-0050 command node for device registration on `push.<domain>`.
pub const REGISTER_DEVICE_NODE: &str = "register-device";
/// XEP-0050 command node for device deregistration on `push.<domain>`.
pub const DISABLE_DEVICE_NODE: &str = "disable-device";

/// FORM_TYPE the Push Service expects on the submitted form. Keep in
/// sync with the server-side handler.
pub const REGISTER_DEVICE_FORM_TYPE: &str = "urn:waddle:push-service:commands:register-device:0";

/// XEP-0004 form field name carrying the assigned XEP-0357 node id in
/// the stage-4 result form.
pub const REGISTER_DEVICE_RESULT_NODE_FIELD: &str = "node";
/// XEP-0004 form field name carrying the assigned Push Service device
/// row id in the stage-4 result form. The caller persists this so the
/// matching `disable-device` round can scope to a single row.
pub const REGISTER_DEVICE_RESULT_DEVICE_ID_FIELD: &str = "device-id";

/// Provider platform discriminator. The wire string maps directly to
/// the `platform` field on the XEP-0004 submit form (XEP-0050 §3).
///
/// `Serialize` / `Deserialize` use the lowercase wire form so this
/// enum can be carried across the WASM boundary directly via
/// `serde_wasm_bindgen` (JS sends `"web"` / `"apns"` / `"fcm"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushPlatform {
    Web,
    Apns,
    Fcm,
}

impl PushPlatform {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Apns => "apns",
            Self::Fcm => "fcm",
        }
    }
}

/// Deployment environment for the provider — `sandbox` distinguishes
/// the APNs `apns_development` endpoint from `apns_production`. Web
/// Push and FCM accept only `prod` today.
///
/// Serde uses the wire-string representations directly (`"prod"` /
/// `"sandbox"`) so the JS chat client passes `environment: "prod"`
/// verbatim through the WASM boundary; unknown values fail at
/// deserialization rather than reaching the IQ builder as typos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PushEnvironment {
    #[serde(rename = "prod")]
    Production,
    #[serde(rename = "sandbox")]
    Sandbox,
}

/// Validated Push Service component JID used by the XEP-0050
/// `register-device` composer. The public FFI/WASM boundaries still
/// accept strings and construct this value at the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushServiceJid(BareJid);

impl PushServiceJid {
    pub fn new(value: impl AsRef<str>) -> Result<Self, PushRegistrationError> {
        let value = value.as_ref().trim();
        if value.is_empty() {
            return Err(PushRegistrationError::MalformedResultForm {
                reason: "push service JID must not be empty".to_string(),
            });
        }
        let jid =
            BareJid::from_str(value).map_err(|_| PushRegistrationError::MalformedResultForm {
                reason: "push service JID must be a parseable bare JID".to_string(),
            })?;
        Ok(Self(jid))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Application identifier submitted to the Push Service. It scopes the
/// stable per-(user, app-id) node, so an empty value is rejected before
/// any IQ is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushAppId(String);

impl PushAppId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, PushRegistrationError> {
        let value = value.as_ref().trim();
        if value.is_empty() {
            return Err(PushRegistrationError::MalformedResultForm {
                reason: "push app id must not be empty".to_string(),
            });
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Typed failures raised by the XEP-0050 `register-device` composer
/// when the Push Service response shape violates the expected
/// protocol choreography.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PushRegistrationError {
    #[error("missing <command/> response during {stage}")]
    MissingCommandResponse { stage: &'static str },
    #[error("unexpected command status during {stage}; expected {expected}")]
    UnexpectedStatus {
        stage: &'static str,
        expected: &'static str,
    },
    #[error("missing sessionid in stage 2")]
    MissingSessionId,
    #[error("stage 4 sessionid did not match the session opened in stage 2")]
    SessionIdMismatch,
    #[error("missing result form in stage 4")]
    MissingResultForm,
    #[error("malformed register-device result form: {reason}")]
    MalformedResultForm { reason: String },
    #[error("XEP-0050 command session expired")]
    SessionExpired,
}

impl PushEnvironment {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Production => "prod",
            Self::Sandbox => "sandbox",
        }
    }
}

/// Platform-discriminated provider credentials. Each variant carries
/// the EXACT subset of fields its provider needs — typed-payloads
/// hard rule (no shared `Option<&str>` bag at the boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushDeviceCredentials {
    WebPush {
        endpoint: String,
        p256dh: String,
        auth: String,
    },
    Apns {
        device_token: String,
    },
    Fcm {
        registration_token: String,
    },
}

impl PushDeviceCredentials {
    pub fn platform(&self) -> PushPlatform {
        match self {
            Self::WebPush { .. } => PushPlatform::Web,
            Self::Apns { .. } => PushPlatform::Apns,
            Self::Fcm { .. } => PushPlatform::Fcm,
        }
    }
}

/// Assigned XEP-0357 node id returned by a successful registration
/// round. Newtype so callers can't confuse it with a free-form string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushNodeId(String);

impl PushNodeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// Push Service-assigned device row id returned by a successful
/// registration round. Newtype mirroring [`PushNodeId`] so the two
/// sibling `urn:waddle:push-*` ids can't be transposed at the WASM /
/// UniFFI boundary where both otherwise collapse to bare `String`s
/// (CLAUDE.md typed-payloads hard rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDeviceId(String);

impl PushDeviceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// Outcome of a successful `register-device` XEP-0050 round. Carries
/// the assigned XEP-0357 node id AND the Push Service-assigned device
/// row id. The caller persists BOTH: node feeds the user-server's
/// XEP-0357 `<enable/>` IQ; device id is the per-device handle for
/// the matching `disable-device` opt-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterDeviceResult {
    pub node: PushNodeId,
    pub device_id: PushDeviceId,
}

/// Transport abstraction used by [`register_push_device`].
///
/// **Intent: test seam, not a stable plugin point.** Production
/// callers MUST use [`crate::client::ClientHandle`] (which blanket-
/// impls this trait via the `native` feature) or the WASM bridge's
/// driver (impl'd in `waddle-xmpp-client-wasm`). The integration
/// test in `tests/register_push_device_against_fake_service.rs`
/// substitutes a scripted lock-step driver — directly threading
/// `ClientHandle` would force the test to spin up a full XMPP
/// transport just to pin the wire shape, and the wasm bridge
/// needs its own impl to convert `JsValue` errors into typed
/// `ClientError`.
///
/// External crates SHOULD NOT impl this trait. If a new driver is
/// genuinely needed (e.g. a different runtime), open an issue first
/// rather than relying on the trait's `pub` visibility — the shape
/// here may evolve as the composer gains new stages.
/// Deliberately NOT `+ Send`: the WASM driver's future wraps a
/// `JsFuture` and is single-threaded by construction. Native callers
/// that need `Send` futures get them structurally because the
/// `ClientHandle` impl's future is `Send` anyway.
pub trait CommandDriver {
    fn send_iq(&self, iq: Element)
        -> impl std::future::Future<Output = ClientResult<Element>> + '_;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl CommandDriver for crate::client::ClientHandle {
    async fn send_iq(&self, iq: Element) -> ClientResult<Element> {
        crate::client::ClientHandle::send_iq(self, iq).await
    }
}

/// Build the stage-3 `<x type='submit'>` form the composer posts back
/// to the Push Service. Exposed publicly so callers and tests can pin
/// the field shape without round-tripping through a fake service.
///
/// `platform` is derived from `credentials` rather than taken
/// separately so a caller can't construct an inconsistent form (e.g.
/// `PushPlatform::Web` with `PushDeviceCredentials::Apns` populated).
/// PR-comment fix: previously this fn took both as arguments and
/// a mismatched pair would silently produce a form the server
/// rejects at runtime.
pub fn build_register_device_submit_form(
    app_id: &str,
    environment: PushEnvironment,
    credentials: &PushDeviceCredentials,
) -> DataForm {
    let mut fields = vec![
        text_single("platform", credentials.platform().as_wire_str()),
        text_single("environment", environment.as_wire_str()),
        text_single("app-id", app_id),
    ];
    match credentials {
        PushDeviceCredentials::WebPush {
            endpoint,
            p256dh,
            auth,
        } => {
            fields.push(text_single("web-push-endpoint", endpoint));
            fields.push(text_single("web-push-p256dh", p256dh));
            fields.push(text_single("web-push-auth", auth));
        }
        PushDeviceCredentials::Apns { device_token } => {
            fields.push(text_single("apns-token", device_token));
        }
        PushDeviceCredentials::Fcm { registration_token } => {
            fields.push(text_single("fcm-token", registration_token));
        }
    }
    DataForm::new(DataFormType::Submit, REGISTER_DEVICE_FORM_TYPE, fields)
}

/// FORM_TYPE for the `disable-device` XEP-0050 command submit form.
pub const DISABLE_DEVICE_FORM_TYPE: &str = "urn:waddle:push-service:commands:disable-device:0";

/// Build the XEP-0004 submit form for the `disable-device` command.
/// Carries the XEP-0357 `node` id and the Push Service-assigned
/// `device-id` so the service can locate the row.
pub fn build_disable_device_submit_form(node: &str, device_id: &str) -> DataForm {
    DataForm::new(
        DataFormType::Submit,
        DISABLE_DEVICE_FORM_TYPE,
        vec![
            text_single("node", node),
            text_single("device-id", device_id),
        ],
    )
}

/// Extract the assigned node id + device id from a `status='completed'`
/// result form. Returns `None` when either field is missing or empty —
/// the chat MUST NOT persist a half-populated record because the
/// follow-up `disable-device` opt-out would have no way to scope.
pub fn parse_register_device_result(form: &DataForm) -> Option<RegisterDeviceResult> {
    // Form-shape gate (PR-comment fix): refuse to parse anything that
    // isn't a stage-4 `<x type='result' FORM_TYPE=REGISTER_DEVICE_FORM_TYPE>`.
    // Without this gate a wrong / drifted form could surface as a
    // missing-field diagnostic ("result form missing 'node' field")
    // when the real cause is a form-confusion attack or a server-side
    // schema drift.
    if form.type_ != DataFormType::Result_ {
        return None;
    }
    if form.form_type() != Some(REGISTER_DEVICE_FORM_TYPE) {
        return None;
    }
    let node = required_form_value(form, REGISTER_DEVICE_RESULT_NODE_FIELD)?;
    let device_id = required_form_value(form, REGISTER_DEVICE_RESULT_DEVICE_ID_FIELD)?;
    Some(RegisterDeviceResult {
        node: PushNodeId(node),
        device_id: PushDeviceId(device_id),
    })
}

fn required_form_value(form: &DataForm, var: &str) -> Option<String> {
    let value = form
        .fields
        .iter()
        .find(|f| f.var.as_deref() == Some(var))?
        .values
        .first()?
        .trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn text_single(var: &str, value: &str) -> Field {
    Field {
        var: Some(var.to_string()),
        type_: FieldType::TextSingle,
        label: None,
        required: false,
        desc: None,
        options: vec![],
        values: vec![value.to_string()],
        media: vec![],
        validate: None,
    }
}

fn push_registration_error(error: PushRegistrationError) -> ClientError {
    ClientError::PushRegistration(error)
}

fn malformed_response(reason: impl Into<String>) -> ClientError {
    push_registration_error(PushRegistrationError::MalformedResultForm {
        reason: reason.into(),
    })
}

fn map_submit_error(error: ClientError) -> ClientError {
    match error {
        ClientError::StanzaError(stanza) if is_xep0050_session_expired(&stanza) => {
            PushRegistrationError::SessionExpired.into()
        }
        other => other,
    }
}

fn is_xep0050_session_expired(stanza: &StanzaError) -> bool {
    // XEP-0050 §4.3 ships `<session-expired/>` as an APPLICATION
    // condition in the commands namespace, alongside the RFC 6120
    // defined condition (`not-allowed` on our server) — so the check
    // must read the application condition, never `condition`.
    stanza
        .application_condition
        .as_ref()
        .is_some_and(|application| {
            application.namespace == "http://jabber.org/protocol/commands"
                && application.name == "session-expired"
        })
}

/// Defense-in-depth check against an IQ response whose `from` doesn't
/// match the addressed `push_service_jid`. RFC 6120 §8.1.2.1 + §10.5:
/// a client SHOULD verify that a response's `from=` matches the
/// request's `to=` to detect spoofed responses arriving on the same
/// stream. The XMPP server-side id-correlation already routes responses
/// by stanza id, but a compromised C2S could deliver an attacker
/// `<iq from='attacker.tld'>` carrying a malicious result form; without
/// this check the composer would happily persist an attacker-issued
/// `node` + `device-id` against the user's push pipeline.
///
/// JID comparison follows RFC 6122 §2.4 (case-insensitive domainpart).
/// Push Service JIDs are pure-domain XEP-0114 components, so a
/// `service_jid/resource` form is rejected even via case-insensitive
/// equality.
fn verify_iq_from_matches(iq: &Element, push_service_jid: &PushServiceJid) -> ClientResult<()> {
    let from = iq.attr("from").ok_or_else(|| {
        malformed_response(
            "Push Service IQ response carries no `from`; RFC 6120 §8.1.2.1 \
             requires components to stamp `from`. Refusing to trust the response.",
        )
    })?;
    if from.eq_ignore_ascii_case(push_service_jid.as_str()) {
        Ok(())
    } else {
        Err(malformed_response(
            "Push Service IQ response `from` does not match the addressed Push \
             Service JID; refusing to trust the response.",
        ))
    }
}

/// Drive the XEP-0050 four-stage `register-device` dance against
/// `push_service_jid`. Returns the assigned [`RegisterDeviceResult`]
/// (node id + device id) on completion. The caller MUST persist BOTH
/// — the node id flows into the user-server XEP-0357 `<enable/>` IQ,
/// the device id scopes the matching `disable-device` opt-out so a
/// single-browser unsubscribe doesn't take down push for sibling
/// devices (APNs, FCM, other browsers) on the same node.
///
/// Errors flow through `ClientResult<StanzaError>` typed values —
/// never stringly-typed payloads (CLAUDE.md typed-payloads hard rule).
pub async fn register_push_device<D: CommandDriver>(
    driver: &D,
    push_service_jid: &PushServiceJid,
    app_id: &PushAppId,
    environment: PushEnvironment,
    credentials: &PushDeviceCredentials,
) -> ClientResult<RegisterDeviceResult> {
    // Stage 1 → 2: execute, expect executing + form back + sessionid.
    let initial = build_xep0050_command_request(
        push_service_jid.as_str(),
        REGISTER_DEVICE_NODE,
        AdHocAction::Execute,
        None,
    );
    let executing_iq = driver.send_iq(initial).await?;
    verify_iq_from_matches(&executing_iq, push_service_jid)?;
    let executing = parse_command_response(&executing_iq)
        .ok_or(PushRegistrationError::MissingCommandResponse { stage: "stage 2" })?;
    if executing.status != AdHocStatus::Executing {
        return Err(PushRegistrationError::UnexpectedStatus {
            stage: "stage 2",
            expected: "executing",
        }
        .into());
    }
    let session_id = executing
        .session_id
        .clone()
        .ok_or(PushRegistrationError::MissingSessionId)?;

    // Stage 3 → 4: submit the platform-specific form, expect
    // completed + result form with the assigned node id.
    let submit_form = build_register_device_submit_form(app_id.as_str(), environment, credentials);
    let complete = build_xep0050_command_request_with_session(
        push_service_jid.as_str(),
        REGISTER_DEVICE_NODE,
        &session_id,
        AdHocAction::Complete,
        Some(submit_form),
    );
    let completed_iq = driver.send_iq(complete).await.map_err(map_submit_error)?;
    verify_iq_from_matches(&completed_iq, push_service_jid)?;
    let completed = parse_command_response(&completed_iq)
        .ok_or(PushRegistrationError::MissingCommandResponse { stage: "stage 4" })?;
    if completed.status != AdHocStatus::Completed {
        return Err(PushRegistrationError::UnexpectedStatus {
            stage: "stage 4",
            expected: "completed",
        }
        .into());
    }
    // XEP-0050 §3.4: the responder echoes the sessionid so the requester
    // can correlate the response to the session it opened in stage 2.
    // Reject a `completed` carrying a different (or absent) sessionid —
    // otherwise a buggy/hostile Push Service, or two register dances
    // crossing on the same stream, could graft the wrong node/device-id
    // onto this registration even though the `from=` check passed.
    match completed.session_id.as_deref() {
        Some(returned) if returned == session_id => {}
        _ => {
            return Err(PushRegistrationError::SessionIdMismatch.into());
        }
    }
    let result_form = completed
        .form
        .ok_or(PushRegistrationError::MissingResultForm)?;
    parse_register_device_result(&result_form).ok_or_else(|| {
        PushRegistrationError::MalformedResultForm {
            reason: "wrong FORM_TYPE, wrong type='result', or missing/empty 'node' / \
                     'device-id' field"
                .to_string(),
        }
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_wire_strings_match_xep0050_form_options() {
        assert_eq!(PushPlatform::Web.as_wire_str(), "web");
        assert_eq!(PushPlatform::Apns.as_wire_str(), "apns");
        assert_eq!(PushPlatform::Fcm.as_wire_str(), "fcm");
        assert_eq!(PushEnvironment::Production.as_wire_str(), "prod");
        assert_eq!(PushEnvironment::Sandbox.as_wire_str(), "sandbox");
    }

    #[test]
    fn credentials_platform_discriminator_matches_variant() {
        assert_eq!(
            PushDeviceCredentials::WebPush {
                endpoint: "e".into(),
                p256dh: "p".into(),
                auth: "a".into()
            }
            .platform(),
            PushPlatform::Web
        );
        assert_eq!(
            PushDeviceCredentials::Apns {
                device_token: "t".into()
            }
            .platform(),
            PushPlatform::Apns
        );
        assert_eq!(
            PushDeviceCredentials::Fcm {
                registration_token: "r".into()
            }
            .platform(),
            PushPlatform::Fcm
        );
    }

    #[test]
    fn build_submit_form_for_web_push_carries_required_fields() {
        let credentials = PushDeviceCredentials::WebPush {
            endpoint: "https://fcm.googleapis.com/wp/abc".to_string(),
            p256dh: "p256-key".to_string(),
            auth: "auth-secret".to_string(),
        };
        let form =
            build_register_device_submit_form("app-web", PushEnvironment::Production, &credentials);
        assert_eq!(form.type_, DataFormType::Submit);
        let values: std::collections::HashMap<&str, &str> = form
            .fields
            .iter()
            .filter_map(|f| Some((f.var.as_deref()?, f.values.first()?.as_str())))
            .collect();
        assert_eq!(
            values.get("FORM_TYPE").copied(),
            Some(REGISTER_DEVICE_FORM_TYPE)
        );
        assert_eq!(values.get("platform").copied(), Some("web"));
        assert_eq!(values.get("environment").copied(), Some("prod"));
        assert_eq!(values.get("app-id").copied(), Some("app-web"));
        assert_eq!(
            values.get("web-push-endpoint").copied(),
            Some("https://fcm.googleapis.com/wp/abc")
        );
        assert_eq!(values.get("web-push-p256dh").copied(), Some("p256-key"));
        assert_eq!(values.get("web-push-auth").copied(), Some("auth-secret"));
    }

    #[test]
    fn build_submit_form_for_apns_omits_web_and_fcm_fields() {
        let credentials = PushDeviceCredentials::Apns {
            device_token: "apns-token".to_string(),
        };
        let form =
            build_register_device_submit_form("app-ios", PushEnvironment::Sandbox, &credentials);
        let vars: Vec<&str> = form
            .fields
            .iter()
            .filter_map(|f| f.var.as_deref())
            .collect();
        assert!(vars.contains(&"apns-token"));
        assert!(!vars.contains(&"web-push-endpoint"));
        assert!(!vars.contains(&"web-push-p256dh"));
        assert!(!vars.contains(&"web-push-auth"));
        assert!(!vars.contains(&"fcm-token"));
    }

    #[test]
    fn build_submit_form_for_fcm_carries_only_fcm_token() {
        let credentials = PushDeviceCredentials::Fcm {
            registration_token: "fcm-reg-token".to_string(),
        };
        let form = build_register_device_submit_form(
            "app-android",
            PushEnvironment::Production,
            &credentials,
        );
        let values: std::collections::HashMap<&str, &str> = form
            .fields
            .iter()
            .filter_map(|f| Some((f.var.as_deref()?, f.values.first()?.as_str())))
            .collect();
        assert_eq!(values.get("platform").copied(), Some("fcm"));
        assert_eq!(values.get("fcm-token").copied(), Some("fcm-reg-token"));
        assert!(!values.contains_key("apns-token"));
        assert!(!values.contains_key("web-push-endpoint"));
    }

    #[test]
    fn parse_completed_result_extracts_node_and_device_id_fields() {
        // Fixture carries multiple fields so a mutation that hard-codes
        // the lookup to the wrong field var couldn't accidentally
        // return the right value.
        let result = DataForm::new(
            DataFormType::Result_,
            REGISTER_DEVICE_FORM_TYPE,
            vec![
                text_single("node", "node-xyz"),
                text_single("device-id", "device-abc"),
                text_single("status", "active-must-not-be-returned"),
            ],
        );
        let outcome = parse_register_device_result(&result).expect("outcome present");
        assert_eq!(outcome.node.as_str(), "node-xyz");
        assert_eq!(outcome.device_id.as_str(), "device-abc");
    }

    #[test]
    fn parse_completed_result_missing_node_returns_none() {
        // Only device-id present — without the node the chat can't
        // wire up the user-server XEP-0357 `<enable/>` IQ, so the
        // composer MUST refuse to surface a half-populated record.
        let result = DataForm::new(
            DataFormType::Result_,
            REGISTER_DEVICE_FORM_TYPE,
            vec![text_single("device-id", "device-abc")],
        );
        assert!(parse_register_device_result(&result).is_none());
    }

    #[test]
    fn parse_completed_result_missing_device_id_returns_none() {
        // Only node present — without device-id the chat can't later
        // scope a per-device `disable-device` opt-out, so this is
        // also a half-populated record we MUST refuse.
        let result = DataForm::new(
            DataFormType::Result_,
            REGISTER_DEVICE_FORM_TYPE,
            vec![text_single("node", "node-xyz")],
        );
        assert!(parse_register_device_result(&result).is_none());
    }

    #[test]
    fn parse_completed_result_empty_node_returns_none() {
        let result = DataForm::new(
            DataFormType::Result_,
            REGISTER_DEVICE_FORM_TYPE,
            vec![
                text_single("node", "   "),
                text_single("device-id", "device-abc"),
            ],
        );
        assert!(parse_register_device_result(&result).is_none());
    }

    #[test]
    fn parse_completed_result_empty_device_id_returns_none() {
        let result = DataForm::new(
            DataFormType::Result_,
            REGISTER_DEVICE_FORM_TYPE,
            vec![
                text_single("node", "node-xyz"),
                text_single("device-id", "   "),
            ],
        );
        assert!(parse_register_device_result(&result).is_none());
    }

    #[test]
    fn push_node_id_round_trips_via_as_str() {
        let result = DataForm::new(
            DataFormType::Result_,
            REGISTER_DEVICE_FORM_TYPE,
            vec![
                text_single("node", "node-roundtrip"),
                text_single("device-id", "device-roundtrip"),
            ],
        );
        let outcome = parse_register_device_result(&result).expect("outcome");
        assert_eq!(outcome.node.as_str(), "node-roundtrip");
        assert_eq!(outcome.node.clone().into_string(), "node-roundtrip");
        assert_eq!(outcome.device_id.as_str(), "device-roundtrip");
        assert_eq!(outcome.device_id.clone().into_string(), "device-roundtrip");
    }

    fn iq_with_from(from_value: Option<&str>) -> Element {
        let mut builder = Element::builder("iq", "jabber:client");
        if let Some(value) = from_value {
            builder = builder.attr(minidom::rxml::xml_ncname!("from").to_owned(), value);
        }
        builder.build()
    }

    #[test]
    fn verify_iq_from_accepts_exact_match() {
        let service_jid = PushServiceJid::new("push.example.com").expect("valid service jid");
        assert!(
            verify_iq_from_matches(&iq_with_from(Some("push.example.com")), &service_jid).is_ok()
        );
    }

    #[test]
    fn verify_iq_from_accepts_case_variation_on_domain() {
        // RFC 6122 §2.4: domainpart comparison is case-insensitive.
        let service_jid = PushServiceJid::new("push.example.com").expect("valid service jid");
        assert!(
            verify_iq_from_matches(&iq_with_from(Some("Push.Example.COM")), &service_jid).is_ok()
        );
    }

    #[test]
    fn verify_iq_from_rejects_absent_from() {
        let service_jid = PushServiceJid::new("push.example.com").expect("valid service jid");
        let err = verify_iq_from_matches(&iq_with_from(None), &service_jid)
            .expect_err("missing `from` must reject");
        match err {
            ClientError::PushRegistration(PushRegistrationError::MalformedResultForm {
                reason,
            }) => {
                assert!(reason.contains("no `from`"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn verify_iq_from_rejects_unrelated_jid() {
        let service_jid = PushServiceJid::new("push.example.com").expect("valid service jid");
        let err = verify_iq_from_matches(&iq_with_from(Some("attacker.tld")), &service_jid)
            .expect_err("unrelated `from` must reject");
        assert!(matches!(err, ClientError::PushRegistration(_)));
    }

    #[test]
    fn verify_iq_from_rejects_prefix_substring_collision() {
        // `push.example.com` MUST NOT match `push.example.com.attacker.tld`.
        let service_jid = PushServiceJid::new("push.example.com").expect("valid service jid");
        let err = verify_iq_from_matches(
            &iq_with_from(Some("push.example.com.attacker.tld")),
            &service_jid,
        )
        .expect_err("substring collision must reject");
        assert!(matches!(err, ClientError::PushRegistration(_)));
    }

    #[test]
    fn verify_iq_from_rejects_empty_from() {
        let service_jid = PushServiceJid::new("push.example.com").expect("valid service jid");
        let err = verify_iq_from_matches(&iq_with_from(Some("")), &service_jid)
            .expect_err("empty `from` must reject");
        assert!(matches!(err, ClientError::PushRegistration(_)));
    }
}
