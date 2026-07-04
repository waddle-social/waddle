//! XEP-0128 disco extension form for the Push Service's current VAPID
//! public key.
//!
//! The Push Service component advertises its active RFC 8292 VAPID keypair
//! via this form so chat clients can discover the public key at runtime
//! instead of receiving it at build time. The form is attached to the
//! component-level `disco#info` response (no `node` attribute); per-node
//! disco results MUST NOT carry it.
//!
//! ## Wire shape (XEP-0128 §2)
//!
//! ```xml
//! <iq type='result'>
//!   <query xmlns='http://jabber.org/protocol/disco#info'>
//!     <identity category='pubsub' type='push' name='Push Service'/>
//!     ...features...
//!     <x xmlns='jabber:x:data' type='result'>
//!       <field var='FORM_TYPE' type='hidden'>
//!         <value>urn:waddle:push:vapid:0</value>
//!       </field>
//!       <field var='public-key'>
//!         <value>BNcRdreALRFXTkOOUHK1...</value>
//!       </field>
//!       <field var='kid'>
//!         <value>9b1f0f3e-1234-4abc-9001-00112233aabb</value>
//!       </field>
//!     </x>
//!   </query>
//! </iq>
//! ```
//!
//! ## Namespace discipline
//!
//! XEP-0357 does not define a FORM_TYPE for the push service's disco
//! extensions, so per the CLAUDE.md XEP-conformance hard rule we mint a
//! `urn:waddle:*` namespace. If the XSF later registers an equivalent
//! FORM_TYPE, this module's constant becomes the migration target.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use minidom::Element;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use thiserror::Error;

use crate::xep::xep0004::{DataForm, Field, FormType, FromElement, ToElement};

use super::types::Kid;

/// FORM_TYPE for the Push Service VAPID disco extension. Custom
/// `urn:waddle:*` namespace per the XEP conformance hard rule.
pub const PUSH_VAPID_FORM_TYPE: &str = "urn:waddle:push:vapid:0";

/// `var` of the public-key field. Value is the base64url-no-pad encoding
/// of the 65-byte uncompressed SEC1 P-256 point (leading `0x04`).
pub const PUSH_VAPID_FIELD_PUBLIC_KEY: &str = "public-key";

/// `var` of the kid field. Value is the canonical hyphenated UUID
/// rendering of the active `push_service_vapid_keys.kid` row.
pub const PUSH_VAPID_FIELD_KID: &str = "kid";

/// Typed advertisement of the Push Service's currently-active VAPID
/// keypair. Crosses the boundary from the push service store to the
/// disco handler as a typed value (typed-payloads hard rule).
#[derive(Debug, Clone)]
pub struct VapidAdvertisement {
    pub public_key: p256::PublicKey,
    pub kid: Kid,
}

impl VapidAdvertisement {
    pub fn new(public_key: p256::PublicKey, kid: Kid) -> Self {
        Self { public_key, kid }
    }

    /// Base64url-no-pad encoding of the 65-byte uncompressed SEC1 point.
    /// Matches the chat-side `applicationServerKey` and the
    /// `Authorization: vapid k=…` parameter format (RFC 8292 §3.2).
    pub fn public_key_base64url(&self) -> String {
        let encoded = self.public_key.to_encoded_point(false);
        URL_SAFE_NO_PAD.encode(encoded.as_bytes())
    }
}

/// Build the XEP-0128 form Element advertising this keypair. The result
/// is appended to the push service's component-level disco#info response
/// via [`build_disco_info_response_with_extensions`].
pub fn push_vapid_disco_form(advertisement: &VapidAdvertisement) -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(PUSH_VAPID_FORM_TYPE))
        .add_field(Field::text_single(
            PUSH_VAPID_FIELD_PUBLIC_KEY,
            advertisement.public_key_base64url(),
        ))
        .add_field(Field::text_single(
            PUSH_VAPID_FIELD_KID,
            advertisement.kid.to_string(),
        ))
        .to_element()
}

/// Parse error for [`parse_push_vapid_disco_form`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PushVapidDiscoFormError {
    #[error("element is not a jabber:x:data form")]
    NotADataForm,
    #[error("FORM_TYPE mismatch: expected `{PUSH_VAPID_FORM_TYPE}`, got {got:?}")]
    WrongFormType { got: Option<String> },
    #[error("missing required field `{0}`")]
    MissingField(&'static str),
    #[error("`public-key` is not valid base64url-no-pad")]
    PublicKeyBase64urlDecode,
    #[error("`public-key` is not a 65-byte uncompressed SEC1 P-256 point (got {got} bytes)")]
    PublicKeyWrongLength { got: usize },
    #[error("`public-key` is not a 0x04-prefixed uncompressed SEC1 point")]
    PublicKeyWrongPrefix,
    #[error("`public-key` is not a valid P-256 point")]
    PublicKeyInvalidPoint,
    #[error("`kid` is not a valid UUID")]
    KidInvalidUuid,
}

/// Inverse of [`push_vapid_disco_form`]. Used by the chat-side fetch path
/// (and the server-side test suite) to recover a typed
/// [`VapidAdvertisement`] from a parsed XEP-0128 form Element. Rejects
/// malformed inputs with a typed error.
pub fn parse_push_vapid_disco_form(
    form_element: &Element,
) -> Result<VapidAdvertisement, PushVapidDiscoFormError> {
    let form =
        DataForm::from_element(form_element).map_err(|_| PushVapidDiscoFormError::NotADataForm)?;
    if form
        .get_form_type_value()
        .is_none_or(|value| value != PUSH_VAPID_FORM_TYPE)
    {
        return Err(PushVapidDiscoFormError::WrongFormType {
            got: form.get_form_type_value().map(str::to_owned),
        });
    }

    let public_key_b64 = form
        .field(PUSH_VAPID_FIELD_PUBLIC_KEY)
        .and_then(|f| f.value())
        .ok_or(PushVapidDiscoFormError::MissingField(
            PUSH_VAPID_FIELD_PUBLIC_KEY,
        ))?;
    let kid_str = form
        .field(PUSH_VAPID_FIELD_KID)
        .and_then(|f| f.value())
        .ok_or(PushVapidDiscoFormError::MissingField(PUSH_VAPID_FIELD_KID))?;

    let public_bytes = URL_SAFE_NO_PAD
        .decode(public_key_b64)
        .map_err(|_| PushVapidDiscoFormError::PublicKeyBase64urlDecode)?;
    if public_bytes.len() != 65 {
        return Err(PushVapidDiscoFormError::PublicKeyWrongLength {
            got: public_bytes.len(),
        });
    }
    if public_bytes[0] != 0x04 {
        return Err(PushVapidDiscoFormError::PublicKeyWrongPrefix);
    }
    let public_key = p256::PublicKey::from_sec1_bytes(&public_bytes)
        .map_err(|_| PushVapidDiscoFormError::PublicKeyInvalidPoint)?;
    let kid_uuid: uuid::Uuid = kid_str
        .parse()
        .map_err(|_| PushVapidDiscoFormError::KidInvalidUuid)?;

    Ok(VapidAdvertisement::new(public_key, Kid(kid_uuid)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::elliptic_curve::rand_core::OsRng;

    fn sample_advertisement() -> VapidAdvertisement {
        let secret = p256::SecretKey::random(&mut OsRng);
        VapidAdvertisement::new(secret.public_key(), Kid::new())
    }

    #[test]
    fn round_trip_advertisement_through_form() {
        let original = sample_advertisement();
        let element = push_vapid_disco_form(&original);
        let parsed = parse_push_vapid_disco_form(&element).expect("round-trips");
        assert_eq!(parsed.kid, original.kid);
        assert_eq!(
            parsed.public_key_base64url(),
            original.public_key_base64url()
        );
    }

    #[test]
    fn public_key_encodes_as_65_byte_uncompressed_sec1() {
        let advertisement = sample_advertisement();
        let encoded = advertisement.public_key_base64url();
        let decoded = URL_SAFE_NO_PAD.decode(&encoded).expect("base64url");
        assert_eq!(decoded.len(), 65);
        assert_eq!(decoded[0], 0x04);
    }

    #[test]
    fn form_carries_correct_form_type() {
        let advertisement = sample_advertisement();
        let element = push_vapid_disco_form(&advertisement);
        let form = DataForm::from_element(&element).expect("parses as data form");
        assert_eq!(form.get_form_type_value(), Some(PUSH_VAPID_FORM_TYPE));
        assert_eq!(form.form_type, FormType::Result);
    }

    #[test]
    fn parse_rejects_wrong_form_type() {
        let element = DataForm::new(FormType::Result)
            .add_field(Field::form_type("urn:waddle:something:else"))
            .add_field(Field::text_single(PUSH_VAPID_FIELD_PUBLIC_KEY, "A"))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_KID,
                uuid::Uuid::nil().to_string(),
            ))
            .to_element();
        let err = parse_push_vapid_disco_form(&element).expect_err("wrong form type");
        assert!(matches!(err, PushVapidDiscoFormError::WrongFormType { .. }));
    }

    #[test]
    fn parse_rejects_missing_public_key() {
        let element = DataForm::new(FormType::Result)
            .add_field(Field::form_type(PUSH_VAPID_FORM_TYPE))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_KID,
                uuid::Uuid::nil().to_string(),
            ))
            .to_element();
        let err = parse_push_vapid_disco_form(&element).expect_err("missing field");
        assert_eq!(
            err,
            PushVapidDiscoFormError::MissingField(PUSH_VAPID_FIELD_PUBLIC_KEY)
        );
    }

    #[test]
    fn parse_rejects_truncated_public_key() {
        let element = DataForm::new(FormType::Result)
            .add_field(Field::form_type(PUSH_VAPID_FORM_TYPE))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_PUBLIC_KEY,
                URL_SAFE_NO_PAD.encode([0x04u8; 32]),
            ))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_KID,
                uuid::Uuid::new_v4().to_string(),
            ))
            .to_element();
        let err = parse_push_vapid_disco_form(&element).expect_err("short key");
        assert!(matches!(
            err,
            PushVapidDiscoFormError::PublicKeyWrongLength { got: 32 }
        ));
    }

    #[test]
    fn parse_rejects_wrong_prefix() {
        let mut bytes = [0u8; 65];
        bytes[0] = 0x02; // compressed-form prefix
        let element = DataForm::new(FormType::Result)
            .add_field(Field::form_type(PUSH_VAPID_FORM_TYPE))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_PUBLIC_KEY,
                URL_SAFE_NO_PAD.encode(bytes),
            ))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_KID,
                uuid::Uuid::new_v4().to_string(),
            ))
            .to_element();
        let err = parse_push_vapid_disco_form(&element).expect_err("wrong prefix");
        assert_eq!(err, PushVapidDiscoFormError::PublicKeyWrongPrefix);
    }

    #[test]
    fn parse_rejects_invalid_kid_uuid() {
        let advertisement = sample_advertisement();
        let element = DataForm::new(FormType::Result)
            .add_field(Field::form_type(PUSH_VAPID_FORM_TYPE))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_PUBLIC_KEY,
                advertisement.public_key_base64url(),
            ))
            .add_field(Field::text_single(PUSH_VAPID_FIELD_KID, "not-a-uuid"))
            .to_element();
        let err = parse_push_vapid_disco_form(&element).expect_err("bad kid");
        assert_eq!(err, PushVapidDiscoFormError::KidInvalidUuid);
    }

    #[test]
    fn parse_rejects_garbage_point_bytes() {
        // 65 bytes, 0x04 prefix, but the X/Y coordinates are all zeros —
        // not a valid P-256 point.
        let mut bytes = [0u8; 65];
        bytes[0] = 0x04;
        let element = DataForm::new(FormType::Result)
            .add_field(Field::form_type(PUSH_VAPID_FORM_TYPE))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_PUBLIC_KEY,
                URL_SAFE_NO_PAD.encode(bytes),
            ))
            .add_field(Field::text_single(
                PUSH_VAPID_FIELD_KID,
                uuid::Uuid::new_v4().to_string(),
            ))
            .to_element();
        let err = parse_push_vapid_disco_form(&element).expect_err("invalid point");
        assert_eq!(err, PushVapidDiscoFormError::PublicKeyInvalidPoint);
    }
}
