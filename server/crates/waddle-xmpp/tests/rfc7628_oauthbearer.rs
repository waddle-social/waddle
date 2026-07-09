//! RFC 7628 OAUTHBEARER conformance tests.
//!
//! Waddle authenticates an already-issued session token. It deliberately
//! exposes no XEP-0493 authorization-server discovery data.

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use waddle_xmpp::auth::oauthbearer::{
    parse_oauthbearer, parse_oauthbearer_error_response, OAuthBearerErrorChallenge,
    OAuthBearerErrorResponse, OAuthBearerParseError, OAuthBearerResult,
};

#[test]
fn empty_authorization_is_an_invalid_credential_not_discovery() {
    for payload in [
        b"n,,\x01host=example.com\x01auth=\x01\x01".as_slice(),
        b"n,,\x01auth=Bearer \x01\x01".as_slice(),
    ] {
        assert!(matches!(
            parse_oauthbearer(payload),
            Ok(OAuthBearerResult::EmptyCredentials)
        ));
    }
    assert_eq!(
        parse_oauthbearer(b"").unwrap_err(),
        OAuthBearerParseError::InvalidGs2Header
    );
}

#[test]
fn bearer_credentials_preserve_bare_authzid_and_hide_token_from_debug() {
    let parsed = parse_oauthbearer(
        b"n,a=alice@example.com,\x01host=example.com\x01port=443\x01auth=bEaReR secret-token\x01\x01",
    )
    .expect("valid RFC 7628 payload");
    let OAuthBearerResult::Credentials(credentials) = parsed else {
        panic!("expected credentials");
    };

    assert_eq!(credentials.token().expose_secret(), "secret-token");
    assert_eq!(
        credentials
            .authorization_identity()
            .map(ToString::to_string)
            .as_deref(),
        Some("alice@example.com")
    );
    assert!(!format!("{credentials:?}").contains("secret-token"));

    assert_eq!(
        parse_oauthbearer(b"n,a=alice@example.com/mobile,\x01auth=Bearer secret-token\x01\x01")
            .unwrap_err(),
        OAuthBearerParseError::InvalidAuthorizationIdentity
    );
    assert_eq!(
        parse_oauthbearer(b"n,a=alice=ZZexample.com,\x01auth=Bearer token\x01\x01").unwrap_err(),
        OAuthBearerParseError::InvalidAuthorizationIdentity
    );
    assert_eq!(
        parse_oauthbearer(b"n,a=alice=3dadmin@example.com,\x01auth=Bearer token\x01\x01")
            .unwrap_err(),
        OAuthBearerParseError::InvalidAuthorizationIdentity
    );
}

#[test]
fn malformed_oauthbearer_shapes_are_rejected() {
    for (payload, expected) in [
        (
            b"garbage".as_slice(),
            OAuthBearerParseError::InvalidGs2Header,
        ),
        (
            b"n,,\x01auth=Bearer token".as_slice(),
            OAuthBearerParseError::MissingTerminator,
        ),
        (
            b"n,,\x01auth=Bearertoken\x01\x01".as_slice(),
            OAuthBearerParseError::UnsupportedAuthorizationScheme,
        ),
        (
            b"n,,\x01auth=Bearer one\x01auth=Bearer two\x01\x01".as_slice(),
            OAuthBearerParseError::DuplicateAttribute,
        ),
        (
            b"n,,\x01host-name=example.com\x01auth=\x01\x01".as_slice(),
            OAuthBearerParseError::MalformedAttribute,
        ),
        (
            b"n,,\x01auth=Bearer token with-space\x01\x01".as_slice(),
            OAuthBearerParseError::InvalidBearerToken,
        ),
        (
            b"n,,\x01auth=Bearer token\nnewline\x01\x01".as_slice(),
            OAuthBearerParseError::InvalidBearerToken,
        ),
        (
            b"n,,\x01auth=Bearer token\0nul\x01\x01".as_slice(),
            OAuthBearerParseError::InvalidBearerToken,
        ),
        (
            b"n,,\x01auth=Bearer token=middle\x01\x01".as_slice(),
            OAuthBearerParseError::InvalidBearerToken,
        ),
    ] {
        assert_eq!(parse_oauthbearer(payload).unwrap_err(), expected);
    }
}

#[test]
fn invalid_token_challenge_contains_only_rfc7628_error_data() {
    let element = OAuthBearerErrorChallenge::invalid_token()
        .to_element()
        .expect("serializable challenge");
    assert_eq!(element.name(), "challenge");
    assert_eq!(element.ns(), waddle_xmpp::ns::SASL);

    let decoded = BASE64_STANDARD
        .decode(element.text())
        .expect("base64 challenge");
    let json: serde_json::Value = serde_json::from_slice(&decoded).expect("JSON challenge");
    assert_eq!(json, serde_json::json!({"status": "invalid_token"}));
    assert!(json.get("openid-configuration").is_none());
}

#[test]
fn failed_exchange_completion_accepts_only_the_rfc7628_octet() {
    assert_eq!(
        parse_oauthbearer_error_response(b"\x01"),
        Ok(OAuthBearerErrorResponse::Acknowledged)
    );
    for invalid in [b"".as_slice(), b"\x01\x01".as_slice(), b"*".as_slice()] {
        assert!(parse_oauthbearer_error_response(invalid).is_err());
    }
    assert_eq!(BASE64_STANDARD.encode(b"\x01"), "AQ==");
}
