//! XEP-0050 ad-hoc command handlers for the push.<domain> service.
//!
//! Two commands live here:
//!
//! - `register-device` (multi-step): stage 1 returns
//!   `status='executing'` + a XEP-0004 form prompt; stage 3 receives a
//!   submitted form carrying typed provider credentials and persists a
//!   `push_devices` row. Stage 4 returns `status='completed'` + a
//!   result form whose `node` field carries the assigned XEP-0357 node
//!   id.
//! - `disable-device` (multi-step, but single-field): stage 3 receives
//!   a submitted form carrying just the `node` id and flips every
//!   device registered against that node + the node itself to
//!   `status='disabled'` for the calling owner. The chat client never
//!   sees per-device IDs under the XEP-0050 cutover.
//!
//! ## Wire contract — `register-device`
//!
//! Submit form FORM_TYPE = [`REGISTER_DEVICE_FORM_TYPE`]
//! (`urn:xmpp:push-service:commands:register-device:0`). Required
//! fields:
//!
//! - `platform` ∈ {`web`, `apns`, `fcm`}.
//! - `environment` ∈ {`prod`, `sandbox`}.
//! - `app_id` — caller-supplied app namespace.
//! - Platform-discriminated provider credentials:
//!   - Web Push: `web-push-endpoint`, `web-push-p256dh`, `web-push-auth`.
//!   - APNs: `apns-token`.
//!   - FCM: `fcm-token`.
//!
//! Result form FORM_TYPE matches the submit form. The lone field is
//! `node`, carrying the assigned XEP-0357 push-service node id.
//!
//! ## Wire contract — `disable-device`
//!
//! Submit form FORM_TYPE = [`DISABLE_DEVICE_FORM_TYPE`]
//! (`urn:xmpp:push-service:commands:disable-device:0`). Required
//! field: `node` (the XEP-0357 push node id). Disables every device
//! registered against that node for the calling owner, then retires
//! the node itself — matching the [`disable_nodes_for_owner`] storage
//! semantics. The chat client never sees per-device IDs under the
//! XEP-0050 cutover, so the wire shape carries only what the client
//! has visibility on. Result form is empty on success.
//!
//! [`disable_nodes_for_owner`]: crate::push_service::DatabasePushServiceStore::disable_nodes_for_owner
//!
//! ## Authorization
//!
//! Both handlers derive the owner BareJID from
//! [`waddle_xmpp::commands::CommandContext::from`]. The underlying
//! storage layer (`crate::push_service`) enforces the
//! owner-binds-to-node invariant via SQL — a caller cannot register a
//! device against another user's node nor disable foreign devices.

use std::sync::Arc;

use jid::Jid;
use uuid::Uuid;
use waddle_xmpp::commands::{CommandContext, CommandResult};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType};
use waddle_xmpp::XmppError;

use crate::push_service::{DatabasePushServiceStore, PushDeviceRegistration};

/// XEP-0050 node identifier for the push device-registration command.
pub const REGISTER_DEVICE_NODE: &str = "register-device";
/// XEP-0050 node identifier for the push device-deregistration command.
pub const DISABLE_DEVICE_NODE: &str = "disable-device";

/// XEP-0004 `FORM_TYPE` value pinning the submit/result form for
/// [`REGISTER_DEVICE_NODE`]. Kept in lockstep with the client-side
/// constant `waddle_xmpp_client::push::REGISTER_DEVICE_FORM_TYPE`.
pub const REGISTER_DEVICE_FORM_TYPE: &str = "urn:xmpp:push-service:commands:register-device:0";
/// XEP-0004 `FORM_TYPE` value pinning the submit form for
/// [`DISABLE_DEVICE_NODE`].
pub const DISABLE_DEVICE_FORM_TYPE: &str = "urn:xmpp:push-service:commands:disable-device:0";

/// XEP-0004 form field carrying the assigned XEP-0357 node id in the
/// stage-4 `register-device` result form.
pub const FIELD_NODE: &str = "node";
/// XEP-0004 form field carrying the platform discriminator
/// (`web` / `apns` / `fcm`).
pub const FIELD_PLATFORM: &str = "platform";
/// XEP-0004 form field carrying the deployment environment
/// (`prod` / `sandbox`).
pub const FIELD_ENVIRONMENT: &str = "environment";
/// XEP-0004 form field carrying the caller's app namespace.
pub const FIELD_APP_ID: &str = "app_id";
/// XEP-0004 form field carrying the Web Push subscription endpoint
/// URL (RFC 8030 §5).
pub const FIELD_WEB_PUSH_ENDPOINT: &str = "web-push-endpoint";
/// XEP-0004 form field carrying the Web Push subscription's
/// SEC1-encoded P-256 public key (RFC 8291 §3.4).
pub const FIELD_WEB_PUSH_P256DH: &str = "web-push-p256dh";
/// XEP-0004 form field carrying the Web Push subscription's
/// 16-byte auth secret (RFC 8291 §3.4).
pub const FIELD_WEB_PUSH_AUTH: &str = "web-push-auth";
/// XEP-0004 form field carrying an APNs device token.
pub const FIELD_APNS_TOKEN: &str = "apns-token";
/// XEP-0004 form field carrying an FCM registration token.
pub const FIELD_FCM_TOKEN: &str = "fcm-token";

/// Wire string for Web Push on [`FIELD_PLATFORM`].
const PLATFORM_WIRE_WEB: &str = "web";
/// Wire string for APNs on [`FIELD_PLATFORM`].
const PLATFORM_WIRE_APNS: &str = "apns";
/// Wire string for FCM on [`FIELD_PLATFORM`].
const PLATFORM_WIRE_FCM: &str = "fcm";

/// Wire string for production environment on [`FIELD_ENVIRONMENT`].
const ENVIRONMENT_WIRE_PROD: &str = "prod";
/// Wire string for sandbox / development environment on
/// [`FIELD_ENVIRONMENT`].
const ENVIRONMENT_WIRE_SANDBOX: &str = "sandbox";

/// Typed parse of the [`REGISTER_DEVICE_NODE`] submit form. Discriminates
/// on `FIELD_PLATFORM`; each variant carries exactly the provider
/// fields its platform needs. Constructed by [`parse_register_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterDeviceRequest {
    /// Web Push (RFC 8030 + RFC 8291) registration.
    WebPush {
        app_id: String,
        environment: String,
        endpoint: String,
        p256dh: String,
        auth: String,
    },
    /// Apple Push Notification service registration.
    Apns {
        app_id: String,
        environment: String,
        device_token: String,
    },
    /// Firebase Cloud Messaging registration.
    Fcm {
        app_id: String,
        environment: String,
        registration_token: String,
    },
}

/// Typed parse of the [`DISABLE_DEVICE_NODE`] submit form. Carries
/// only the push-service node id; the storage layer flips every
/// device on that node to `disabled` for the calling owner and
/// retires the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisableDeviceRequest {
    pub node: String,
}

/// Register both push-service ad-hoc commands on `registry`. Invoked
/// once at startup from `http.rs`.
pub async fn register(
    registry: &waddle_xmpp::commands::CommandRegistry,
    push_service: Arc<DatabasePushServiceStore>,
) {
    let store_for_register = Arc::clone(&push_service);
    registry
        .register(REGISTER_DEVICE_NODE, "Push · Register device", move |ctx| {
            let store = Arc::clone(&store_for_register);
            async move { handle_register_device(ctx, store).await }
        })
        .await;

    let store_for_disable = Arc::clone(&push_service);
    registry
        .register(DISABLE_DEVICE_NODE, "Push · Disable device", move |ctx| {
            let store = Arc::clone(&store_for_disable);
            async move { handle_disable_device(ctx, store).await }
        })
        .await;
}

async fn handle_register_device(
    ctx: CommandContext,
    push_service: Arc<DatabasePushServiceStore>,
) -> CommandResult {
    // Stage 1 → 2: `action='execute'` with no submitted form ⇒ return
    // the form prompt. The registry has already minted a session id by
    // the time the handler sees the context, so the executing arm
    // simply echoes back the prompt with an empty `session_id` and
    // the registry rewrites it (`registry.rs` Executing branch).
    //
    // Stage 3 → 4: `action='complete'` carrying the submitted form ⇒
    // parse, persist, return the result form.
    let submitted = ctx.command.form.as_ref();
    let Some(submitted) = submitted else {
        return CommandResult::Executing {
            form: build_register_device_form_prompt(),
            session_id: String::new(),
            notes: vec![],
        };
    };

    if !matches!(submitted.form_type, FormType::Submit) {
        return CommandResult::Error(XmppError::bad_request(Some(
            "register-device requires a submit form".to_string(),
        )));
    }

    let request = match parse_register_request(submitted) {
        Ok(request) => request,
        Err(error) => return CommandResult::Error(XmppError::bad_request(Some(error))),
    };

    let owner = match bare_jid_from_caller(&ctx.from) {
        Some(jid) => jid,
        None => {
            return CommandResult::Error(XmppError::not_authorized(Some(
                "register-device requires a bound resource".to_string(),
            )));
        }
    };

    // Allocate (or reuse) the XEP-0357 push node bound to (owner,
    // app_id). The custom-namespace handler used a separate
    // `ensure-node` round-trip; the XEP-0050 dance folds that step
    // into stage 3 so the chat client never sees the node id until
    // the result form returns it. Storage layout is unchanged.
    let app_id = request.app_id().to_string();
    let push_node = match push_service.ensure_node(&owner, &app_id).await {
        Ok(node) => node,
        Err(error) => return CommandResult::Error(error),
    };

    let device_id = generate_device_id();
    let registration = build_registration(&request, &device_id, push_node.node());
    let device = match push_service.upsert_device(&owner, registration).await {
        Ok(device) => device,
        Err(error) => return CommandResult::Error(error),
    };

    CommandResult::Completed {
        form: Some(build_register_device_result_form(device.node())),
        notes: vec![],
    }
}

async fn handle_disable_device(
    ctx: CommandContext,
    push_service: Arc<DatabasePushServiceStore>,
) -> CommandResult {
    let submitted = ctx.command.form.as_ref();
    let Some(submitted) = submitted else {
        return CommandResult::Executing {
            form: build_disable_device_form_prompt(),
            session_id: String::new(),
            notes: vec![],
        };
    };

    if !matches!(submitted.form_type, FormType::Submit) {
        return CommandResult::Error(XmppError::bad_request(Some(
            "disable-device requires a submit form".to_string(),
        )));
    }

    let request = match parse_disable_request(submitted) {
        Ok(request) => request,
        Err(error) => return CommandResult::Error(XmppError::bad_request(Some(error))),
    };

    let owner = match bare_jid_from_caller(&ctx.from) {
        Some(jid) => jid,
        None => {
            return CommandResult::Error(XmppError::not_authorized(Some(
                "disable-device requires a bound resource".to_string(),
            )));
        }
    };

    match push_service
        .disable_nodes_for_owner(&owner, Some(&request.node))
        .await
    {
        Ok(0) => CommandResult::Error(XmppError::item_not_found(Some(
            "Requested Push Service node not found".to_string(),
        ))),
        Ok(_) => CommandResult::Completed {
            form: None,
            notes: vec![],
        },
        Err(error) => CommandResult::Error(error),
    }
}

/// Build the stage-2 `<x type='form'>` prompt the service returns when
/// the caller executes `register-device`. The form documents the
/// required fields so a generic XEP-0050 client can prompt the user
/// with no out-of-band knowledge.
pub fn build_register_device_form_prompt() -> DataForm {
    use waddle_xmpp::xep::xep0004::FieldType;

    DataForm::new(FormType::Form)
        .with_title("Register push device")
        .add_instructions(
            "Submit the platform-specific provider credentials to register a new push device.",
        )
        .add_field(Field::form_type(REGISTER_DEVICE_FORM_TYPE))
        .add_field(
            Field::new(FIELD_PLATFORM, FieldType::ListSingle)
                .with_label("Provider platform")
                .with_required()
                .add_option(waddle_xmpp::xep::xep0004::FieldOption::with_label(
                    "Web Push",
                    PLATFORM_WIRE_WEB,
                ))
                .add_option(waddle_xmpp::xep::xep0004::FieldOption::with_label(
                    "Apple Push (APNs)",
                    PLATFORM_WIRE_APNS,
                ))
                .add_option(waddle_xmpp::xep::xep0004::FieldOption::with_label(
                    "Firebase Cloud Messaging",
                    PLATFORM_WIRE_FCM,
                )),
        )
        .add_field(
            Field::new(FIELD_ENVIRONMENT, FieldType::ListSingle)
                .with_label("Deployment environment")
                .with_required()
                .add_option(waddle_xmpp::xep::xep0004::FieldOption::with_label(
                    "Production",
                    ENVIRONMENT_WIRE_PROD,
                ))
                .add_option(waddle_xmpp::xep::xep0004::FieldOption::with_label(
                    "Sandbox / development",
                    ENVIRONMENT_WIRE_SANDBOX,
                )),
        )
        .add_field(
            Field::new(FIELD_APP_ID, FieldType::TextSingle)
                .with_label("Application identifier")
                .with_required(),
        )
        .add_field(
            Field::new(FIELD_WEB_PUSH_ENDPOINT, FieldType::TextSingle)
                .with_label("Web Push endpoint URL (web only)"),
        )
        .add_field(
            Field::new(FIELD_WEB_PUSH_P256DH, FieldType::TextSingle)
                .with_label("Web Push P-256 public key (web only)"),
        )
        .add_field(
            Field::new(FIELD_WEB_PUSH_AUTH, FieldType::TextSingle)
                .with_label("Web Push auth secret (web only)"),
        )
        .add_field(
            Field::new(FIELD_APNS_TOKEN, FieldType::TextSingle)
                .with_label("APNs token (apns only)"),
        )
        .add_field(
            Field::new(FIELD_FCM_TOKEN, FieldType::TextSingle)
                .with_label("FCM registration token (fcm only)"),
        )
}

/// Build the stage-2 `<x type='form'>` prompt the service returns when
/// the caller executes `disable-device`.
pub fn build_disable_device_form_prompt() -> DataForm {
    use waddle_xmpp::xep::xep0004::FieldType;

    DataForm::new(FormType::Form)
        .with_title("Disable push device")
        .add_instructions("Submit the push node id of the device that should be disabled.")
        .add_field(Field::form_type(DISABLE_DEVICE_FORM_TYPE))
        .add_field(
            Field::new(FIELD_NODE, FieldType::TextSingle)
                .with_label("Push node id")
                .with_required(),
        )
}

/// Build the stage-4 `<x type='result'>` form the service returns
/// after a successful `register-device` round. Carries the assigned
/// XEP-0357 node id under [`FIELD_NODE`].
pub fn build_register_device_result_form(node: &str) -> DataForm {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(REGISTER_DEVICE_FORM_TYPE))
        .add_field(Field::text_single(FIELD_NODE, node))
}

/// Parse a submitted [`REGISTER_DEVICE_NODE`] form into the typed
/// request enum. All validation lives here so the handler never sees a
/// `&str` carrying protocol semantics.
pub fn parse_register_request(form: &DataForm) -> Result<RegisterDeviceRequest, String> {
    let app_id = require_value(form, FIELD_APP_ID)?.to_string();
    let environment_raw = require_value(form, FIELD_ENVIRONMENT)?;
    let environment = match environment_raw {
        ENVIRONMENT_WIRE_PROD | ENVIRONMENT_WIRE_SANDBOX => environment_raw.to_string(),
        other => {
            return Err(format!(
                "{FIELD_ENVIRONMENT} must be '{ENVIRONMENT_WIRE_PROD}' or '{ENVIRONMENT_WIRE_SANDBOX}', got '{other}'"
            ))
        }
    };
    let platform_raw = require_value(form, FIELD_PLATFORM)?;
    match platform_raw {
        PLATFORM_WIRE_WEB => {
            let endpoint = require_value(form, FIELD_WEB_PUSH_ENDPOINT)?.to_string();
            let p256dh = require_value(form, FIELD_WEB_PUSH_P256DH)?.to_string();
            let auth = require_value(form, FIELD_WEB_PUSH_AUTH)?.to_string();
            Ok(RegisterDeviceRequest::WebPush {
                app_id,
                environment,
                endpoint,
                p256dh,
                auth,
            })
        }
        PLATFORM_WIRE_APNS => {
            let device_token = require_value(form, FIELD_APNS_TOKEN)?.to_string();
            Ok(RegisterDeviceRequest::Apns {
                app_id,
                environment,
                device_token,
            })
        }
        PLATFORM_WIRE_FCM => {
            let registration_token = require_value(form, FIELD_FCM_TOKEN)?.to_string();
            Ok(RegisterDeviceRequest::Fcm {
                app_id,
                environment,
                registration_token,
            })
        }
        other => Err(format!(
            "{FIELD_PLATFORM} must be one of '{PLATFORM_WIRE_WEB}', '{PLATFORM_WIRE_APNS}', '{PLATFORM_WIRE_FCM}', got '{other}'"
        )),
    }
}

/// Parse a submitted [`DISABLE_DEVICE_NODE`] form into the typed
/// request.
pub fn parse_disable_request(form: &DataForm) -> Result<DisableDeviceRequest, String> {
    let node = require_value(form, FIELD_NODE)?.to_string();
    Ok(DisableDeviceRequest { node })
}

impl RegisterDeviceRequest {
    pub fn app_id(&self) -> &str {
        match self {
            Self::WebPush { app_id, .. } | Self::Apns { app_id, .. } | Self::Fcm { app_id, .. } => {
                app_id
            }
        }
    }

    pub fn environment(&self) -> &str {
        match self {
            Self::WebPush { environment, .. }
            | Self::Apns { environment, .. }
            | Self::Fcm { environment, .. } => environment,
        }
    }
}

fn build_registration(
    request: &RegisterDeviceRequest,
    device_id: &str,
    node: &str,
) -> PushDeviceRegistration {
    match request {
        RegisterDeviceRequest::WebPush {
            environment,
            endpoint,
            p256dh,
            auth,
            ..
        } => PushDeviceRegistration::new(
            device_id,
            node,
            crate::push_service::PushDevicePlatform::Web,
            environment.clone(),
        )
        .with_provider_endpoint(Some(endpoint.clone()))
        .with_provider_token(Some(auth.clone()))
        .with_provider_key_material(Some(p256dh.clone())),
        RegisterDeviceRequest::Apns {
            environment,
            device_token,
            ..
        } => PushDeviceRegistration::new(
            device_id,
            node,
            crate::push_service::PushDevicePlatform::Apns,
            environment.clone(),
        )
        .with_provider_token(Some(device_token.clone())),
        RegisterDeviceRequest::Fcm {
            environment,
            registration_token,
            ..
        } => PushDeviceRegistration::new(
            device_id,
            node,
            crate::push_service::PushDevicePlatform::Fcm,
            environment.clone(),
        )
        .with_provider_token(Some(registration_token.clone())),
    }
}

fn require_value<'a>(form: &'a DataForm, var: &str) -> Result<&'a str, String> {
    form.get_value(var)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing required form field '{var}'"))
}

fn bare_jid_from_caller(from: &Jid) -> Option<jid::BareJid> {
    Some(from.to_bare())
}

/// Mint a fresh device id. The custom-namespace handler accepted a
/// caller-supplied `device-id`; under the XEP-0050 cutover the service
/// owns the id so the chat / mobile client never has to invent one
/// before the server has accepted the registration. Stored in
/// `push_devices.device_id`; relayed back to the caller inside the
/// result form's wider context isn't needed because the chat client
/// never needs to address an individual device by id (disables flow
/// through the same node id the service surfaces in the result form).
fn generate_device_id() -> String {
    format!("urn:waddle:push-device:{}", Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit_form_with(fields: Vec<Field>) -> DataForm {
        let mut form =
            DataForm::new(FormType::Submit).add_field(Field::form_type(REGISTER_DEVICE_FORM_TYPE));
        for field in fields {
            form = form.add_field(field);
        }
        form
    }

    #[test]
    fn parse_web_push_request_carries_required_provider_fields() {
        let form = submit_form_with(vec![
            Field::text_single(FIELD_PLATFORM, PLATFORM_WIRE_WEB),
            Field::text_single(FIELD_ENVIRONMENT, ENVIRONMENT_WIRE_PROD),
            Field::text_single(FIELD_APP_ID, "web-app"),
            Field::text_single(FIELD_WEB_PUSH_ENDPOINT, "https://relay.example/wp/1"),
            Field::text_single(FIELD_WEB_PUSH_P256DH, "p256-key"),
            Field::text_single(FIELD_WEB_PUSH_AUTH, "auth-secret"),
        ]);
        let request = parse_register_request(&form).expect("parses");
        match request {
            RegisterDeviceRequest::WebPush {
                app_id,
                environment,
                endpoint,
                p256dh,
                auth,
            } => {
                assert_eq!(app_id, "web-app");
                assert_eq!(environment, ENVIRONMENT_WIRE_PROD);
                assert_eq!(endpoint, "https://relay.example/wp/1");
                assert_eq!(p256dh, "p256-key");
                assert_eq!(auth, "auth-secret");
            }
            other => panic!("expected WebPush, got {other:?}"),
        }
    }

    #[test]
    fn parse_web_push_request_rejects_missing_endpoint() {
        let form = submit_form_with(vec![
            Field::text_single(FIELD_PLATFORM, PLATFORM_WIRE_WEB),
            Field::text_single(FIELD_ENVIRONMENT, ENVIRONMENT_WIRE_PROD),
            Field::text_single(FIELD_APP_ID, "web-app"),
            Field::text_single(FIELD_WEB_PUSH_P256DH, "p256-key"),
            Field::text_single(FIELD_WEB_PUSH_AUTH, "auth-secret"),
        ]);
        let err = parse_register_request(&form).expect_err("missing endpoint must reject");
        assert!(err.contains(FIELD_WEB_PUSH_ENDPOINT), "{err}");
    }

    #[test]
    fn parse_apns_request_carries_only_apns_token() {
        let form = submit_form_with(vec![
            Field::text_single(FIELD_PLATFORM, PLATFORM_WIRE_APNS),
            Field::text_single(FIELD_ENVIRONMENT, ENVIRONMENT_WIRE_SANDBOX),
            Field::text_single(FIELD_APP_ID, "ios-app"),
            Field::text_single(FIELD_APNS_TOKEN, "apns-device-token"),
        ]);
        let request = parse_register_request(&form).expect("parses");
        match request {
            RegisterDeviceRequest::Apns {
                app_id,
                environment,
                device_token,
            } => {
                assert_eq!(app_id, "ios-app");
                assert_eq!(environment, ENVIRONMENT_WIRE_SANDBOX);
                assert_eq!(device_token, "apns-device-token");
            }
            other => panic!("expected Apns, got {other:?}"),
        }
    }

    #[test]
    fn parse_fcm_request_carries_registration_token() {
        let form = submit_form_with(vec![
            Field::text_single(FIELD_PLATFORM, PLATFORM_WIRE_FCM),
            Field::text_single(FIELD_ENVIRONMENT, ENVIRONMENT_WIRE_PROD),
            Field::text_single(FIELD_APP_ID, "android-app"),
            Field::text_single(FIELD_FCM_TOKEN, "fcm-reg-token"),
        ]);
        let request = parse_register_request(&form).expect("parses");
        match request {
            RegisterDeviceRequest::Fcm {
                app_id,
                environment,
                registration_token,
            } => {
                assert_eq!(app_id, "android-app");
                assert_eq!(environment, ENVIRONMENT_WIRE_PROD);
                assert_eq!(registration_token, "fcm-reg-token");
            }
            other => panic!("expected Fcm, got {other:?}"),
        }
    }

    #[test]
    fn parse_register_request_rejects_unknown_platform() {
        let form = submit_form_with(vec![
            Field::text_single(FIELD_PLATFORM, "telegram"),
            Field::text_single(FIELD_ENVIRONMENT, ENVIRONMENT_WIRE_PROD),
            Field::text_single(FIELD_APP_ID, "any"),
        ]);
        let err = parse_register_request(&form).expect_err("unknown platform must reject");
        assert!(err.contains(FIELD_PLATFORM), "{err}");
    }

    #[test]
    fn parse_register_request_rejects_unknown_environment() {
        let form = submit_form_with(vec![
            Field::text_single(FIELD_PLATFORM, PLATFORM_WIRE_WEB),
            Field::text_single(FIELD_ENVIRONMENT, "staging"),
            Field::text_single(FIELD_APP_ID, "any"),
            Field::text_single(FIELD_WEB_PUSH_ENDPOINT, "https://x.example/wp"),
            Field::text_single(FIELD_WEB_PUSH_P256DH, "p"),
            Field::text_single(FIELD_WEB_PUSH_AUTH, "a"),
        ]);
        let err = parse_register_request(&form).expect_err("unknown environment must reject");
        assert!(err.contains(FIELD_ENVIRONMENT), "{err}");
    }

    #[test]
    fn parse_disable_request_round_trip() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(DISABLE_DEVICE_FORM_TYPE))
            .add_field(Field::text_single(FIELD_NODE, "node-xyz"));
        let request = parse_disable_request(&form).expect("parses");
        assert_eq!(request.node, "node-xyz");
    }

    #[test]
    fn parse_disable_request_rejects_missing_node() {
        let form =
            DataForm::new(FormType::Submit).add_field(Field::form_type(DISABLE_DEVICE_FORM_TYPE));
        let err = parse_disable_request(&form).expect_err("missing node must reject");
        assert!(err.contains(FIELD_NODE), "{err}");
    }

    #[test]
    fn register_device_result_form_carries_node_field() {
        let form = build_register_device_result_form("urn:waddle:push-node:abc");
        assert!(matches!(form.form_type, FormType::Result));
        assert_eq!(form.get_form_type_value(), Some(REGISTER_DEVICE_FORM_TYPE));
        assert_eq!(form.get_value(FIELD_NODE), Some("urn:waddle:push-node:abc"));
    }

    #[test]
    fn generate_device_id_is_unique_per_call() {
        let a = generate_device_id();
        let b = generate_device_id();
        assert_ne!(a, b);
        assert!(a.starts_with("urn:waddle:push-device:"));
    }

    #[test]
    fn register_device_form_prompt_carries_form_type_and_required_fields() {
        let form = build_register_device_form_prompt();
        assert!(matches!(form.form_type, FormType::Form));
        assert_eq!(form.get_form_type_value(), Some(REGISTER_DEVICE_FORM_TYPE));
        for var in [
            FIELD_PLATFORM,
            FIELD_ENVIRONMENT,
            FIELD_APP_ID,
            FIELD_WEB_PUSH_ENDPOINT,
            FIELD_WEB_PUSH_P256DH,
            FIELD_WEB_PUSH_AUTH,
            FIELD_APNS_TOKEN,
            FIELD_FCM_TOKEN,
        ] {
            assert!(
                form.field(var).is_some(),
                "register-device form prompt missing field '{var}'"
            );
        }
    }

    #[test]
    fn disable_device_form_prompt_carries_node_field() {
        let form = build_disable_device_form_prompt();
        assert!(matches!(form.form_type, FormType::Form));
        assert_eq!(form.get_form_type_value(), Some(DISABLE_DEVICE_FORM_TYPE));
        assert!(form.field(FIELD_NODE).is_some());
    }
}
