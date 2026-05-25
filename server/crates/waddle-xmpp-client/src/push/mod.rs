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

use minidom::Element;
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};

use crate::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
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

/// Outcome of a successful `register-device` XEP-0050 round. Carries
/// the assigned XEP-0357 node id AND the Push Service-assigned device
/// row id. The caller persists BOTH: node feeds the user-server's
/// XEP-0357 `<enable/>` IQ; device id is the per-device handle for
/// the matching `disable-device` opt-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterDeviceResult {
    pub node: PushNodeId,
    pub device_id: String,
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
#[allow(async_fn_in_trait)]
pub trait CommandDriver {
    async fn send_iq(&self, iq: Element) -> ClientResult<Element>;
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
pub fn build_register_device_submit_form(
    app_id: &str,
    platform: PushPlatform,
    environment: PushEnvironment,
    credentials: &PushDeviceCredentials,
) -> DataForm {
    let mut fields = vec![
        text_single("platform", platform.as_wire_str()),
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
    let node = required_form_value(form, REGISTER_DEVICE_RESULT_NODE_FIELD)?;
    let device_id = required_form_value(form, REGISTER_DEVICE_RESULT_DEVICE_ID_FIELD)?;
    Some(RegisterDeviceResult {
        node: PushNodeId(node),
        device_id,
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

fn protocol_error(text: &str) -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "bad-request".to_string(),
        text: Some(text.to_string()),
    })
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
    push_service_jid: &str,
    app_id: &str,
    environment: PushEnvironment,
    credentials: &PushDeviceCredentials,
) -> ClientResult<RegisterDeviceResult> {
    // Stage 1 → 2: execute, expect executing + form back + sessionid.
    let initial = build_xep0050_command_request(
        push_service_jid,
        REGISTER_DEVICE_NODE,
        AdHocAction::Execute,
        None,
    );
    let executing_iq = driver.send_iq(initial).await?;
    let executing = parse_command_response(&executing_iq)
        .ok_or_else(|| protocol_error("expected <command/> response in stage 2"))?;
    if executing.status != AdHocStatus::Executing {
        return Err(protocol_error("expected status='executing' in stage 2"));
    }
    let session_id = executing
        .session_id
        .clone()
        .ok_or_else(|| protocol_error("missing sessionid in stage 2"))?;

    // Stage 3 → 4: submit the platform-specific form, expect
    // completed + result form with the assigned node id.
    let submit_form =
        build_register_device_submit_form(app_id, credentials.platform(), environment, credentials);
    let complete = build_xep0050_command_request_with_session(
        push_service_jid,
        REGISTER_DEVICE_NODE,
        &session_id,
        AdHocAction::Complete,
        Some(submit_form),
    );
    let completed_iq = driver.send_iq(complete).await?;
    let completed = parse_command_response(&completed_iq)
        .ok_or_else(|| protocol_error("expected <command/> response in stage 4"))?;
    if completed.status != AdHocStatus::Completed {
        return Err(protocol_error("expected status='completed' in stage 4"));
    }
    let result_form = completed
        .form
        .ok_or_else(|| protocol_error("missing result form in stage 4"))?;
    parse_register_device_result(&result_form)
        .ok_or_else(|| protocol_error("result form missing 'node' field"))
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
        let form = build_register_device_submit_form(
            "app-web",
            PushPlatform::Web,
            PushEnvironment::Production,
            &credentials,
        );
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
        let form = build_register_device_submit_form(
            "app-ios",
            PushPlatform::Apns,
            PushEnvironment::Sandbox,
            &credentials,
        );
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
            PushPlatform::Fcm,
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
        assert_eq!(outcome.device_id, "device-abc");
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
        assert_eq!(outcome.device_id, "device-roundtrip");
    }
}
