//! Chat-side parser for the Push Service's XEP-0128 VAPID disco extension.
//!
//! The Waddle Push Service component advertises its current VAPID public
//! key + kid in the component-level disco#info response via a
//! `urn:waddle:push:vapid:0` data form (XEP-0128 §2). This module
//! consumes the typed [`DiscoInfoResult`] returned by
//! [`super::parse_disco_info_result`] and surfaces the typed
//! [`PushVapidAdvertisement`] the chat needs to pass into
//! `pushManager.subscribe({ applicationServerKey })`.
//!
//! The wire FORM_TYPE + field-name constants here mirror the server-side
//! definitions in `waddle-xmpp/src/push/disco.rs`. Both halves are
//! intentionally **independent copies** of the wire identifiers — the
//! lower client crate (used in wasm) does not depend on the server-side
//! `waddle-xmpp` crate, so we duplicate the three string constants and
//! keep both ends honest via the integration test suite that parses the
//! actual server response.
//!
//! ## Validation
//!
//! The parser asserts the wire-shape invariants the browser would reject
//! anyway, but at a layer where the chat can surface a meaningful error
//! instead of a `pushManager.subscribe()` rejection:
//!
//!   * `public-key` decodes as base64url-no-pad
//!   * decoded length is exactly 65 bytes (uncompressed SEC1 P-256 point)
//!   * leading byte is `0x04` (uncompressed-point marker)
//!   * `kid` is a parseable UUID
//!
//! It does not assert P-256 point validity — that's the browser's job,
//! and importing a curve crate into the chat client doubles its WASM
//! bundle for one error path.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jid::{BareJid, Jid};
use minidom::Element;
use thiserror::Error;
use uuid::Uuid;

use super::{parse_disco_info_result, DiscoInfoResult};

/// FORM_TYPE for the Push Service's VAPID disco extension. Mirrors
/// `waddle_xmpp::push::disco::PUSH_VAPID_FORM_TYPE`.
pub const PUSH_VAPID_FORM_TYPE: &str = "urn:waddle:push:vapid:0";

/// `var` of the public-key field. Mirrors
/// `waddle_xmpp::push::disco::PUSH_VAPID_FIELD_PUBLIC_KEY`.
pub const PUSH_VAPID_FIELD_PUBLIC_KEY: &str = "public-key";

/// `var` of the kid field. Mirrors
/// `waddle_xmpp::push::disco::PUSH_VAPID_FIELD_KID`.
pub const PUSH_VAPID_FIELD_KID: &str = "kid";

/// Length of an uncompressed SEC1 P-256 point in bytes (1-byte prefix +
/// 32-byte X + 32-byte Y).
const SEC1_UNCOMPRESSED_P256_LEN: usize = 65;
/// Uncompressed-point marker per SEC1 §2.3.3.
const SEC1_UNCOMPRESSED_PREFIX: u8 = 0x04;

/// Typed view of the Push Service's currently-active VAPID keypair, as
/// recovered from the chat-side parse of the disco#info response. The
/// `public_key` value is preserved in its wire form (base64url-no-pad)
/// because that's exactly what the browser needs to subscribe with —
/// re-encoding from raw bytes is a needless serialization round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushVapidAdvertisement {
    /// Base64url-encoded uncompressed P-256 VAPID public key.
    pub public_key_base64url: String,
    /// UUID identifying the advertised VAPID signing key.
    pub kid: Uuid,
}

/// Outcomes for [`parse_push_vapid_from_disco_info`]. `NotAdvertised` is
/// a non-error: the server simply doesn't run a Web Push provider on this
/// deployment. Other variants flag a malformed advertisement; callers
/// should refuse to subscribe rather than degrade silently.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PushVapidDiscoParseError {
    #[error("Push Service disco#info does not advertise a `{PUSH_VAPID_FORM_TYPE}` form")]
    /// Push Service disco#info does not advertise a ``PUSH_VAPID_FORM_TYPE`` form.
    NotAdvertised,
    #[error("missing required field `{0}`")]
    /// Missing required field ``0``.
    MissingField(&'static str),
    #[error("`public-key` is not valid base64url-no-pad")]
    /// `public-key` is not valid base64url-no-pad.
    PublicKeyBase64urlDecode,
    #[error(
        "`public-key` is not a {SEC1_UNCOMPRESSED_P256_LEN}-byte uncompressed SEC1 P-256 point (got {got} bytes)"
    )]
    /// The decoded VAPID public key does not have the required uncompressed P-256 length.
    PublicKeyWrongLength {
        #[doc = "Number of decoded public-key bytes that were received."]
        got: usize,
    },
    #[error("`public-key` is not a 0x04-prefixed uncompressed SEC1 point")]
    /// `public-key` is not a 0x04-prefixed uncompressed SEC1 point.
    PublicKeyWrongPrefix,
    #[error("`kid` is not a valid UUID")]
    /// `kid` is not a valid UUID.
    KidInvalidUuid,
}

/// Extract the typed VAPID advertisement from a parsed disco#info result.
/// Errors are non-fatal in the sense that an unrecognized server (no
/// VAPID form) just means Web Push isn't available — but malformed forms
/// MUST surface as errors so the chat refuses to subscribe instead of
/// guessing at a half-decoded key.
///
/// # Errors
///
/// Returns a typed discovery error if the VAPID form is absent or has missing fields, an invalid base64url key, the wrong key length/prefix, or an invalid UUID key identifier.
pub fn parse_push_vapid_from_disco_info(
    disco: &DiscoInfoResult,
) -> Result<PushVapidAdvertisement, PushVapidDiscoParseError> {
    let form = disco
        .forms
        .iter()
        .find(|form| form.form_type.as_deref() == Some(PUSH_VAPID_FORM_TYPE))
        .ok_or(PushVapidDiscoParseError::NotAdvertised)?;

    let public_key_b64 = form
        .fields
        .iter()
        .find(|field| field.var == PUSH_VAPID_FIELD_PUBLIC_KEY)
        .and_then(|field| field.values.first())
        .ok_or(PushVapidDiscoParseError::MissingField(
            PUSH_VAPID_FIELD_PUBLIC_KEY,
        ))?;
    let kid_value = form
        .fields
        .iter()
        .find(|field| field.var == PUSH_VAPID_FIELD_KID)
        .and_then(|field| field.values.first())
        .ok_or(PushVapidDiscoParseError::MissingField(PUSH_VAPID_FIELD_KID))?;

    let decoded = URL_SAFE_NO_PAD
        .decode(public_key_b64)
        .map_err(|_| PushVapidDiscoParseError::PublicKeyBase64urlDecode)?;
    if decoded.len() != SEC1_UNCOMPRESSED_P256_LEN {
        return Err(PushVapidDiscoParseError::PublicKeyWrongLength { got: decoded.len() });
    }
    if decoded[0] != SEC1_UNCOMPRESSED_PREFIX {
        return Err(PushVapidDiscoParseError::PublicKeyWrongPrefix);
    }
    let kid = kid_value
        .parse::<Uuid>()
        .map_err(|_| PushVapidDiscoParseError::KidInvalidUuid)?;
    Ok(PushVapidAdvertisement {
        public_key_base64url: public_key_b64.clone(),
        kid,
    })
}

/// Outcomes for [`parse_push_vapid_reply`]: the reply-level checks in
/// front of [`parse_push_vapid_from_disco_info`].
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PushVapidReplyError {
    /// The reply is not stamped by the queried service (RFC 6120
    /// §8.1.2.1 requires components to stamp `from`). A key from anyone
    /// else must never be trusted to authenticate pushes.
    #[error("Push Service disco#info reply is not from the queried service")]
    WrongOrigin,
    #[error("Push Service disco#info reply carries no disco#info <query/>")]
    /// Push Service disco#info reply carries no disco#info `<query/>`.
    MissingQuery,
    #[error(transparent)]
    /// The expected service replied, but its VAPID advertisement failed validation.
    Advertisement(#[from] PushVapidDiscoParseError),
}

/// Parse the Push Service's VAPID advertisement out of its disco#info
/// reply `reply`, refusing any reply whose `from` is not exactly
/// `service` (a bare component JID, never a resource).
///
/// # Errors
///
/// Returns `WrongOrigin` for a reply from a different service, `MissingQuery` for a missing discovery payload, or the nested VAPID advertisement validation error.
pub fn parse_push_vapid_reply(
    reply: &Element,
    service: &BareJid,
) -> Result<PushVapidAdvertisement, PushVapidReplyError> {
    let from = reply
        .attr("from")
        .and_then(|from| from.parse::<Jid>().ok())
        .ok_or(PushVapidReplyError::WrongOrigin)?;
    if from.resource().is_some() || from.to_bare() != *service {
        return Err(PushVapidReplyError::WrongOrigin);
    }
    let disco = parse_disco_info_result(reply, service.as_str())
        .ok_or(PushVapidReplyError::MissingQuery)?;
    Ok(parse_push_vapid_from_disco_info(&disco)?)
}

#[cfg(test)]
mod reply_tests {
    use super::*;
    use crate::discovery::{CLIENT_NS, DATA_FORMS_NS, DISCO_INFO_NS};
    use minidom::rxml::xml_ncname;

    const KID: &str = "0b3c1d7e-8f4a-4d2b-9c6e-1a2b3c4d5e6f";

    fn field(var: &str, value: &str) -> Element {
        Element::builder("field", DATA_FORMS_NS)
            .attr(xml_ncname!("var").to_owned(), var)
            .append(
                Element::builder("value", DATA_FORMS_NS)
                    .append(value)
                    .build(),
            )
            .build()
    }

    /// XEP-0068: a FORM_TYPE is only a context indicator when hidden.
    fn form_type_field() -> Element {
        Element::builder("field", DATA_FORMS_NS)
            .attr(xml_ncname!("var").to_owned(), "FORM_TYPE")
            .attr(xml_ncname!("type").to_owned(), "hidden")
            .append(
                Element::builder("value", DATA_FORMS_NS)
                    .append(PUSH_VAPID_FORM_TYPE)
                    .build(),
            )
            .build()
    }

    fn public_key() -> String {
        let mut bytes = [0u8; 65];
        bytes[0] = 0x04;
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn reply(from: Option<&str>) -> Element {
        let form = Element::builder("x", DATA_FORMS_NS)
            .attr(xml_ncname!("type").to_owned(), "result")
            .append(form_type_field())
            .append(field(PUSH_VAPID_FIELD_PUBLIC_KEY, &public_key()))
            .append(field(PUSH_VAPID_FIELD_KID, KID))
            .build();
        let query = Element::builder("query", DISCO_INFO_NS)
            .append(form)
            .build();
        let mut iq = Element::builder("iq", CLIENT_NS)
            .attr(xml_ncname!("type").to_owned(), "result")
            .attr(xml_ncname!("id").to_owned(), "disco-1");
        if let Some(from) = from {
            iq = iq.attr(xml_ncname!("from").to_owned(), from);
        }
        iq.append(query).build()
    }

    fn service() -> BareJid {
        BareJid::new("push.example.com").expect("valid service jid")
    }

    #[test]
    fn accepts_a_reply_from_the_queried_service() {
        let parsed = parse_push_vapid_reply(&reply(Some("push.example.com")), &service())
            .expect("reply from the service parses");
        assert_eq!(parsed.public_key_base64url, public_key());
        assert_eq!(parsed.kid.to_string(), KID);
    }

    #[test]
    fn refuses_a_reply_without_from() {
        assert_eq!(
            parse_push_vapid_reply(&reply(None), &service()),
            Err(PushVapidReplyError::WrongOrigin),
        );
    }

    #[test]
    fn refuses_a_reply_from_another_jid() {
        for from in [
            "evil.example.com",
            "push.example.com/res",
            "user@push.example.com",
        ] {
            assert_eq!(
                parse_push_vapid_reply(&reply(Some(from)), &service()),
                Err(PushVapidReplyError::WrongOrigin),
                "{from}",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::types::{DiscoDataField, DiscoDataForm, DiscoInfoResult};

    fn disco_with_form(form: DiscoDataForm) -> DiscoInfoResult {
        DiscoInfoResult {
            jid: "push.example.com".to_string(),
            node: None,
            identities: Vec::new(),
            features: Vec::new(),
            forms: vec![form],
        }
    }

    fn make_form(public_key_b64: &str, kid: &str) -> DiscoDataForm {
        DiscoDataForm {
            form_type: Some(PUSH_VAPID_FORM_TYPE.to_string()),
            fields: vec![
                DiscoDataField {
                    var: "FORM_TYPE".to_string(),
                    values: vec![PUSH_VAPID_FORM_TYPE.to_string()],
                },
                DiscoDataField {
                    var: PUSH_VAPID_FIELD_PUBLIC_KEY.to_string(),
                    values: vec![public_key_b64.to_string()],
                },
                DiscoDataField {
                    var: PUSH_VAPID_FIELD_KID.to_string(),
                    values: vec![kid.to_string()],
                },
            ],
        }
    }

    fn valid_b64() -> String {
        let mut bytes = [0u8; 65];
        bytes[0] = 0x04;
        for (index, byte) in bytes.iter_mut().enumerate().skip(1) {
            *byte = (index as u8).wrapping_mul(7);
        }
        URL_SAFE_NO_PAD.encode(bytes)
    }

    #[test]
    fn parses_well_formed_advertisement() {
        let kid = Uuid::new_v4();
        let b64 = valid_b64();
        let disco = disco_with_form(make_form(&b64, &kid.to_string()));
        let parsed = parse_push_vapid_from_disco_info(&disco).expect("parses");
        assert_eq!(parsed.kid, kid);
        assert_eq!(parsed.public_key_base64url, b64);
    }

    #[test]
    fn returns_not_advertised_when_form_absent() {
        let disco = DiscoInfoResult {
            jid: "push.example.com".to_string(),
            node: None,
            identities: Vec::new(),
            features: Vec::new(),
            forms: Vec::new(),
        };
        let err = parse_push_vapid_from_disco_info(&disco).expect_err("no form");
        assert_eq!(err, PushVapidDiscoParseError::NotAdvertised);
    }

    #[test]
    fn rejects_missing_public_key_field() {
        let mut form = make_form(&valid_b64(), &Uuid::new_v4().to_string());
        form.fields.retain(|f| f.var != PUSH_VAPID_FIELD_PUBLIC_KEY);
        let disco = disco_with_form(form);
        let err = parse_push_vapid_from_disco_info(&disco).expect_err("missing");
        assert_eq!(
            err,
            PushVapidDiscoParseError::MissingField(PUSH_VAPID_FIELD_PUBLIC_KEY)
        );
    }

    #[test]
    fn rejects_short_public_key() {
        let short = URL_SAFE_NO_PAD.encode([0x04u8; 32]);
        let disco = disco_with_form(make_form(&short, &Uuid::new_v4().to_string()));
        let err = parse_push_vapid_from_disco_info(&disco).expect_err("too short");
        assert_eq!(
            err,
            PushVapidDiscoParseError::PublicKeyWrongLength { got: 32 }
        );
    }

    #[test]
    fn rejects_wrong_prefix() {
        let mut bytes = [0u8; 65];
        bytes[0] = 0x02;
        let b64 = URL_SAFE_NO_PAD.encode(bytes);
        let disco = disco_with_form(make_form(&b64, &Uuid::new_v4().to_string()));
        let err = parse_push_vapid_from_disco_info(&disco).expect_err("bad prefix");
        assert_eq!(err, PushVapidDiscoParseError::PublicKeyWrongPrefix);
    }

    #[test]
    fn rejects_invalid_kid_uuid() {
        let disco = disco_with_form(make_form(&valid_b64(), "not-a-uuid"));
        let err = parse_push_vapid_from_disco_info(&disco).expect_err("bad kid");
        assert_eq!(err, PushVapidDiscoParseError::KidInvalidUuid);
    }

    #[test]
    fn rejects_non_base64url_payload() {
        let disco = disco_with_form(make_form("!!! not base64 !!!", &Uuid::new_v4().to_string()));
        let err = parse_push_vapid_from_disco_info(&disco).expect_err("bad b64");
        assert_eq!(err, PushVapidDiscoParseError::PublicKeyBase64urlDecode);
    }

    /// XEP-0068 §4.3 (Incorrectly Specified FORM_TYPE): a non-hidden
    /// FORM_TYPE field MUST be ignored as a context indicator. The
    /// wire-level disco parser already strips the form_type when the
    /// originating field isn't `type='hidden'`, which surfaces here as
    /// `form_type: None` → `NotAdvertised`. This test pins that
    /// behavior at the typed boundary so a regression in
    /// `parse_disco_data_form` is caught by the chat-side suite.
    #[test]
    fn rejects_non_hidden_form_type_field() {
        use minidom::Element;

        use crate::discovery::parsing::parse_disco_info_result;
        use crate::discovery::DATA_FORMS_NS;
        use crate::discovery::DISCO_INFO_NS;

        let bytes = {
            let mut b = [0u8; 65];
            b[0] = 0x04;
            for (i, v) in b.iter_mut().enumerate().skip(1) {
                *v = (i as u8).wrapping_mul(7);
            }
            b
        };
        let public_key_b64 = URL_SAFE_NO_PAD.encode(bytes);
        let kid = Uuid::new_v4().to_string();
        // Same wire shape as the legitimate form, but FORM_TYPE is
        // emitted WITHOUT `type='hidden'`.
        let form = Element::builder("x", DATA_FORMS_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("field", DATA_FORMS_NS)
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                    // Deliberately omit `type='hidden'`.
                    .append(
                        Element::builder("value", DATA_FORMS_NS)
                            .append(PUSH_VAPID_FORM_TYPE)
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", DATA_FORMS_NS)
                    .attr(
                        minidom::rxml::xml_ncname!("var").to_owned(),
                        PUSH_VAPID_FIELD_PUBLIC_KEY,
                    )
                    .append(
                        Element::builder("value", DATA_FORMS_NS)
                            .append(&public_key_b64[..])
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("field", DATA_FORMS_NS)
                    .attr(
                        minidom::rxml::xml_ncname!("var").to_owned(),
                        PUSH_VAPID_FIELD_KID,
                    )
                    .append(
                        Element::builder("value", DATA_FORMS_NS)
                            .append(&kid[..])
                            .build(),
                    )
                    .build(),
            )
            .build();
        let iq = Element::builder("iq", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("query", DISCO_INFO_NS)
                    .append(form)
                    .build(),
            )
            .build();

        let disco = parse_disco_info_result(&iq, "push.example.com")
            .expect("disco parses despite the non-hidden FORM_TYPE");
        let err = parse_push_vapid_from_disco_info(&disco)
            .expect_err("non-hidden FORM_TYPE must NOT be honored as a context indicator");
        assert_eq!(err, PushVapidDiscoParseError::NotAdvertised);
    }
}
