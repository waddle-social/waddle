//! Web Push dispatch bridge for the publish-job worker.
//!
//! Owns the translations between this crate's XMPP-side state (the
//! XEP-0357 `<notification>` payload XML and sealed `push_devices`
//! rows) and the typed `waddle_xmpp::push` foundations
//! (`SubscriptionKeys`, `PushEnvelope`, `encrypt::encrypt`,
//! `VapidSigner::sign`, `WebPushSender::send`).
//!
//! Keeping it as a submodule keeps `push_service.rs` focused on
//! durable-queue mechanics; the cross-crate seam lives here so a
//! future reviewer can read the full Web Push delivery flow in one
//! file.

use std::sync::Arc;

use minidom::Element;
use url::Url;
use waddle_xmpp::push::envelope::{push_class_for_db_value, push_topic_for, PushEnvelope};
use waddle_xmpp::push::types::{SubscriptionKeys, VapidSub, WebPushOutcome};
use waddle_xmpp::push::vapid::{aud_for, vapid_k_header, VapidSigner};
use waddle_xmpp::push::{encrypt, WebPushRequest, WebPushSender};
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;

use super::{PushDevicePlatform, PushSecretCipher};

/// `urn:waddle:push:context:0` — the typed routing envelope the chat
/// notification publisher emits as a child of `<notification>`. Mirrors
/// the constant defined in [`crate::notification_outbox`].
const NS_WADDLE_PUSH_CONTEXT: &str = "urn:waddle:push:context:0";
/// XEP-0357 §4 data-form FORM_TYPE value.
const XEP0357_SUMMARY_FORM_TYPE: &str = "urn:xmpp:push:summary";
/// XEP-0004 data-forms namespace.
const NS_DATA_FORMS: &str = "jabber:x:data";

/// Parsed XEP-0357 §4 notification payload as we emit it on the wire.
#[derive(Debug, Clone)]
pub(crate) struct ParsedPushPayload {
    /// XEP-0357 §4 `message-count` summary form field. `None` if the
    /// publisher omitted it.
    pub message_count: Option<u64>,
    /// `<context conversation='...'/>` — bare JID of the DM peer or
    /// MUC room.
    pub conversation: String,
    /// `<context thread='...'/>` — XEP-0201 thread id, when the
    /// publisher set one.
    pub thread: Option<String>,
    /// `<context class='...'/>` — db-form notification class such as
    /// `"dm"`, `"personal_mention"`, `"channel_mention"`, etc.
    pub class: String,
}

/// Parse the serialized `<notification>` element. Strictly validates
/// the outer name+namespace (the XEP-0357 §4 envelope) and then
/// extracts the typed `<context>` and `message-count` fields.
pub(crate) fn parse_publish_payload(payload_xml: &str) -> Result<ParsedPushPayload, XmppError> {
    let payload: Element = payload_xml.parse().map_err(|err: minidom::Error| {
        XmppError::internal(format!("XEP-0357 payload is not valid XML: {err}"))
    })?;
    if !payload.is("notification", NS_PUSH) {
        return Err(XmppError::internal(
            "XEP-0357 payload is not <notification xmlns='urn:xmpp:push:0'>".to_string(),
        ));
    }
    let context = payload
        .children()
        .find(|child| child.is("context", NS_WADDLE_PUSH_CONTEXT))
        .ok_or_else(|| {
            XmppError::internal(
                "XEP-0357 payload missing <context xmlns='urn:waddle:push:context:0'/>".to_string(),
            )
        })?;
    let conversation = context
        .attr("conversation")
        .ok_or_else(|| {
            XmppError::internal("waddle <context> missing conversation attribute".to_string())
        })?
        .to_string();
    let class = context
        .attr("class")
        .ok_or_else(|| XmppError::internal("waddle <context> missing class attribute".to_string()))?
        .to_string();
    let thread = context
        .attr("thread")
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    let message_count = parse_summary_message_count(&payload);
    Ok(ParsedPushPayload {
        message_count,
        conversation,
        thread,
        class,
    })
}

fn parse_summary_message_count(notification: &Element) -> Option<u64> {
    let form = notification
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))?;
    let mut saw_form_type = false;
    let mut count: Option<u64> = None;
    for field in form.children().filter(|c| c.is("field", NS_DATA_FORMS)) {
        let var = field.attr("var").unwrap_or_default();
        if var == "FORM_TYPE" {
            let v = field
                .get_child("value", NS_DATA_FORMS)
                .map(|el| el.text())
                .unwrap_or_default();
            if v == XEP0357_SUMMARY_FORM_TYPE {
                saw_form_type = true;
            }
        } else if var == "message-count" {
            let v = field
                .get_child("value", NS_DATA_FORMS)
                .map(|el| el.text())
                .unwrap_or_default();
            count = v.parse().ok();
        }
    }
    if saw_form_type {
        count
    } else {
        None
    }
}

/// A `push_devices` row with its sealed provider material — exactly
/// what the worker needs to materialize a typed Web Push subscription
/// outside the DB transaction.
#[derive(Debug, Clone)]
pub(crate) struct SealedActiveDevice {
    pub device_id: String,
    pub platform: PushDevicePlatform,
    pub sealed_endpoint: Option<String>,
    pub sealed_auth: Option<String>,
    pub sealed_key_material: Option<String>,
}

/// Resolved Web Push target ready for `encrypt` + `send`.
pub(crate) struct WebPushTarget {
    pub endpoint: Url,
    pub keys: SubscriptionKeys,
}

/// Reason a device row was not converted into a [`WebPushTarget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceSkipReason {
    /// Not the Web Push platform (APNS/FCM are handled by sibling
    /// provider slices #529 / #530).
    WrongPlatform,
    /// Missing sealed material (endpoint, p256dh, or auth).
    MissingProviderMaterial,
    /// Unseal failed (root key drift or tampering).
    UnsealFailed,
    /// Endpoint URL did not parse as `https://...`.
    InvalidEndpoint,
    /// `p256dh` / `auth_secret` did not decode/validate.
    InvalidSubscriptionKeys,
}

impl WebPushTarget {
    /// Unseal a `push_devices` row into a typed `WebPushTarget`.
    /// Returns `Ok(Err(reason))` for rows that are intentionally
    /// non-deliverable (e.g. wrong platform, missing material), so the
    /// worker can record a clear typed attempt status without bubbling
    /// the row up as a hard error.
    pub fn try_from_sealed(
        device: &SealedActiveDevice,
        cipher: &PushSecretCipher,
    ) -> Result<WebPushTarget, DeviceSkipReason> {
        if device.platform != PushDevicePlatform::Web {
            return Err(DeviceSkipReason::WrongPlatform);
        }
        let (Some(ep_sealed), Some(auth_sealed), Some(p256dh_sealed)) = (
            device.sealed_endpoint.as_ref(),
            device.sealed_auth.as_ref(),
            device.sealed_key_material.as_ref(),
        ) else {
            return Err(DeviceSkipReason::MissingProviderMaterial);
        };
        let ep_plain = cipher
            .open(ep_sealed)
            .map_err(|_| DeviceSkipReason::UnsealFailed)?;
        let auth_plain = cipher
            .open(auth_sealed)
            .map_err(|_| DeviceSkipReason::UnsealFailed)?;
        let p256dh_plain = cipher
            .open(p256dh_sealed)
            .map_err(|_| DeviceSkipReason::UnsealFailed)?;
        let endpoint = Url::parse(&ep_plain).map_err(|_| DeviceSkipReason::InvalidEndpoint)?;
        if endpoint.scheme() != "https" {
            return Err(DeviceSkipReason::InvalidEndpoint);
        }
        let keys = SubscriptionKeys::from_base64url(&p256dh_plain, &auth_plain)
            .map_err(|_| DeviceSkipReason::InvalidSubscriptionKeys)?;
        Ok(WebPushTarget { endpoint, keys })
    }
}

/// Encrypt + sign + send one Web Push message and return its typed
/// outcome. Errors only on internal bugs (encrypt/sign failure, aud
/// derivation failure); transport-layer failures land in
/// [`WebPushOutcome`] variants.
pub(crate) async fn dispatch_one_web_push(
    target: &WebPushTarget,
    parsed: &ParsedPushPayload,
    item_id: &str,
    signer: &Arc<dyn VapidSigner>,
    sub: &VapidSub,
    sender: &Arc<dyn WebPushSender>,
) -> Result<WebPushOutcome, XmppError> {
    let push_class = push_class_for_db_value(&parsed.class);
    let envelope = PushEnvelope::new(
        &parsed.class,
        &parsed.conversation,
        parsed.thread.as_deref(),
        item_id,
        parsed.message_count,
    );
    let plaintext = envelope.to_plaintext();
    let payload = encrypt::encrypt(&target.keys, &plaintext, push_class.bucket_size())
        .map_err(|err| XmppError::internal(format!("web push encrypt failed: {err}")))?;
    let aud = aud_for(&target.endpoint)
        .map_err(|err| XmppError::internal(format!("web push aud derive failed: {err}")))?;
    let jwt = signer
        .sign(&aud, sub)
        .map_err(|err| XmppError::internal(format!("web push JWT sign failed: {err}")))?;
    let public_key = signer.current_public_key();
    let key_b64u = vapid_k_header(&public_key);
    let topic = push_topic_for(push_class, &parsed.conversation);
    let outcome = sender
        .send(WebPushRequest {
            endpoint: &target.endpoint,
            payload: &payload,
            vapid_jwt: &jwt,
            vapid_public_key_b64u: &key_b64u,
            topic: Some(&topic),
            ttl: push_class.ttl(),
            urgency: push_class.urgency(),
        })
        .await;
    Ok(outcome)
}

/// Map a typed [`WebPushOutcome`] to the string value persisted in
/// `push_delivery_attempts.status`. Kept in one place so the worker
/// and the future XEP-0357 §6 cleanup chain agree on the wire format.
pub(crate) fn outcome_to_attempt_status(outcome: &WebPushOutcome) -> &'static str {
    match outcome {
        WebPushOutcome::Delivered { .. } => ATTEMPT_STATUS_WEB_DELIVERED,
        WebPushOutcome::SubscriptionGone { .. } => ATTEMPT_STATUS_WEB_GONE,
        WebPushOutcome::ClockSkew { .. } => ATTEMPT_STATUS_WEB_CLOCK_SKEW,
        WebPushOutcome::RateLimited { .. } => ATTEMPT_STATUS_WEB_RATE_LIMITED,
        WebPushOutcome::PayloadTooLarge { .. } => ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE,
        WebPushOutcome::BadRequest { .. } => ATTEMPT_STATUS_WEB_BAD_REQUEST,
        WebPushOutcome::Transient { .. } => ATTEMPT_STATUS_WEB_TRANSIENT,
    }
}

/// Status string written when a device row was non-deliverable (wrong
/// platform, missing material, etc.) so an operator can see why.
pub(crate) fn skip_reason_to_attempt_status(reason: DeviceSkipReason) -> &'static str {
    match reason {
        DeviceSkipReason::WrongPlatform => ATTEMPT_STATUS_FAKE_SENT_NON_WEB,
        DeviceSkipReason::MissingProviderMaterial => ATTEMPT_STATUS_WEB_MISSING_MATERIAL,
        DeviceSkipReason::UnsealFailed => ATTEMPT_STATUS_WEB_UNSEAL_FAILED,
        DeviceSkipReason::InvalidEndpoint => ATTEMPT_STATUS_WEB_INVALID_ENDPOINT,
        DeviceSkipReason::InvalidSubscriptionKeys => ATTEMPT_STATUS_WEB_INVALID_KEYS,
    }
}

// Typed attempt-status constants. Kept here next to the
// outcome-to-status mapper so the wire format and the typed
// `WebPushOutcome` enum stay in lockstep.
pub(crate) const ATTEMPT_STATUS_WEB_DELIVERED: &str = "web-delivered";
pub(crate) const ATTEMPT_STATUS_WEB_GONE: &str = "web-gone";
pub(crate) const ATTEMPT_STATUS_WEB_CLOCK_SKEW: &str = "web-clock-skew";
pub(crate) const ATTEMPT_STATUS_WEB_RATE_LIMITED: &str = "web-rate-limited";
pub(crate) const ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE: &str = "web-payload-too-large";
pub(crate) const ATTEMPT_STATUS_WEB_BAD_REQUEST: &str = "web-bad-request";
pub(crate) const ATTEMPT_STATUS_WEB_TRANSIENT: &str = "web-transient";
pub(crate) const ATTEMPT_STATUS_WEB_MISSING_MATERIAL: &str = "web-missing-material";
pub(crate) const ATTEMPT_STATUS_WEB_UNSEAL_FAILED: &str = "web-unseal-failed";
pub(crate) const ATTEMPT_STATUS_WEB_INVALID_ENDPOINT: &str = "web-invalid-endpoint";
pub(crate) const ATTEMPT_STATUS_WEB_INVALID_KEYS: &str = "web-invalid-keys";
/// Recorded for non-Web platforms (APNS/FCM) until #529/#530 land
/// their real senders. Mirrors the historical `fake-sent` marker.
pub(crate) const ATTEMPT_STATUS_FAKE_SENT_NON_WEB: &str = "fake-sent";

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::push::envelope::PushClass;

    fn build_notification_xml(
        class: &str,
        conversation: &str,
        thread: Option<&str>,
        message_count: Option<u64>,
    ) -> String {
        let mut form = format!(
            r#"<x xmlns='jabber:x:data' type='result'><field var='FORM_TYPE' type='hidden'><value>{XEP0357_SUMMARY_FORM_TYPE}</value></field>"#
        );
        if let Some(count) = message_count {
            form.push_str(&format!(
                "<field var='message-count'><value>{count}</value></field>"
            ));
        }
        form.push_str("</x>");
        let thread_attr = thread.map(|t| format!(" thread='{t}'")).unwrap_or_default();
        let context = format!(
            "<context xmlns='{NS_WADDLE_PUSH_CONTEXT}' conversation='{conversation}' class='{class}'{thread_attr}/>"
        );
        format!("<notification xmlns='{NS_PUSH}'>{form}{context}</notification>")
    }

    #[test]
    fn parse_publish_payload_extracts_all_fields() {
        let xml = build_notification_xml(
            "personal_mention",
            "room@conf.example.com",
            Some("thread-1"),
            Some(7),
        );
        let parsed = parse_publish_payload(&xml).expect("valid payload");
        assert_eq!(parsed.class, "personal_mention");
        assert_eq!(parsed.conversation, "room@conf.example.com");
        assert_eq!(parsed.thread.as_deref(), Some("thread-1"));
        assert_eq!(parsed.message_count, Some(7));
    }

    #[test]
    fn parse_publish_payload_treats_missing_thread_as_none() {
        let xml = build_notification_xml("dm", "alice@example.com", None, Some(1));
        let parsed = parse_publish_payload(&xml).expect("valid payload");
        assert!(parsed.thread.is_none());
    }

    #[test]
    fn parse_publish_payload_rejects_wrong_root() {
        let xml = "<message xmlns='jabber:client'/>";
        let err = parse_publish_payload(xml).unwrap_err();
        assert!(err.to_string().contains("not <notification"));
    }

    #[test]
    fn parse_publish_payload_requires_context() {
        let xml = format!("<notification xmlns='{NS_PUSH}'/>");
        let err = parse_publish_payload(&xml).unwrap_err();
        assert!(err.to_string().contains("missing <context"));
    }

    #[test]
    fn parse_publish_payload_ignores_non_summary_form() {
        // Form is present but FORM_TYPE doesn't match XEP-0357 §4.
        // message-count must be ignored — we don't trust counters
        // from unknown forms.
        let xml = format!(
            r#"<notification xmlns='{NS_PUSH}'>
              <x xmlns='jabber:x:data' type='result'>
                <field var='FORM_TYPE' type='hidden'><value>some:other:form</value></field>
                <field var='message-count'><value>42</value></field>
              </x>
              <context xmlns='{NS_WADDLE_PUSH_CONTEXT}' conversation='c' class='dm'/>
            </notification>"#
        );
        let parsed = parse_publish_payload(&xml).expect("payload parses");
        assert_eq!(parsed.message_count, None);
    }

    #[test]
    fn outcome_to_attempt_status_covers_every_variant() {
        // Exhaustive — if a future variant lands the match in
        // `outcome_to_attempt_status` will not compile.
        for (outcome, expected) in [
            (
                WebPushOutcome::Delivered { status: 201 },
                ATTEMPT_STATUS_WEB_DELIVERED,
            ),
            (
                WebPushOutcome::SubscriptionGone { status: 410 },
                ATTEMPT_STATUS_WEB_GONE,
            ),
            (
                WebPushOutcome::ClockSkew { status: 401 },
                ATTEMPT_STATUS_WEB_CLOCK_SKEW,
            ),
            (
                WebPushOutcome::RateLimited {
                    status: 429,
                    retry_after: None,
                },
                ATTEMPT_STATUS_WEB_RATE_LIMITED,
            ),
            (
                WebPushOutcome::PayloadTooLarge { status: 413 },
                ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE,
            ),
            (
                WebPushOutcome::BadRequest { status: 400 },
                ATTEMPT_STATUS_WEB_BAD_REQUEST,
            ),
            (
                WebPushOutcome::Transient {
                    kind: waddle_xmpp::push::types::TransientFailure::Network,
                },
                ATTEMPT_STATUS_WEB_TRANSIENT,
            ),
        ] {
            assert_eq!(outcome_to_attempt_status(&outcome), expected);
        }
    }

    #[test]
    fn skip_reason_status_uses_fake_sent_for_non_web() {
        assert_eq!(
            skip_reason_to_attempt_status(DeviceSkipReason::WrongPlatform),
            "fake-sent",
            "non-web platforms continue to record fake-sent until #529/#530"
        );
    }

    #[test]
    fn push_class_for_db_value_routes_dm_classes_to_dm_bucket() {
        assert_eq!(push_class_for_db_value("dm"), PushClass::DirectMessage);
        assert_eq!(
            push_class_for_db_value("dm_mention"),
            PushClass::DirectMessage
        );
        assert_eq!(
            push_class_for_db_value("personal_mention"),
            PushClass::Mention
        );
    }
}
