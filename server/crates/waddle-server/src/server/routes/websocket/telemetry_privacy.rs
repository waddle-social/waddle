use waddle_xmpp::{
    auth::SaslMechanism,
    protocol::{frame::ParseError, ConnectionPhase},
};

pub(super) fn parse_error_category(error: &ParseError) -> &'static str {
    match error {
        ParseError::Empty => "empty",
        ParseError::TooLarge => "too-large",
        ParseError::InvalidXml(_) => "invalid-xml",
        ParseError::UnknownRoot(_) => "unknown-root",
        ParseError::MalformedSasl(_) => "malformed-sasl",
        ParseError::InvalidSaslInitialResponseEncoding { .. } => "invalid-sasl-initial-response",
        ParseError::InvalidSaslResponseEncoding => "invalid-sasl-response",
        ParseError::MalformedIsrAuthenticate(_) => "malformed-isr-authenticate",
        ParseError::InvalidStanza { kind: "iq", .. } => "invalid-iq",
        ParseError::InvalidStanza {
            kind: "message", ..
        } => "invalid-message",
        ParseError::InvalidStanza {
            kind: "presence", ..
        } => "invalid-presence",
        ParseError::InvalidStanza { .. } => "invalid-stanza",
    }
}

pub(super) const fn sasl_mechanism_category(mechanism: &SaslMechanism) -> &'static str {
    match mechanism {
        SaslMechanism::Plain => "plain",
        SaslMechanism::OAuthBearer => "oauthbearer",
        SaslMechanism::ScramSha256 => "scram-sha-256",
        SaslMechanism::Unsupported => "unsupported",
    }
}

pub(super) const fn connection_phase_category(phase: &ConnectionPhase) -> &'static str {
    match phase {
        ConnectionPhase::Unauthenticated => "unauthenticated",
        ConnectionPhase::ScramPending { .. } => "scram-pending",
        ConnectionPhase::OAuthBearerInitialResponsePending => "oauthbearer-response-pending",
        ConnectionPhase::OAuthBearerErrorPending => "oauthbearer-error-pending",
        ConnectionPhase::Authenticated { .. } => "authenticated",
        ConnectionPhase::Ready { resumed: true, .. } => "ready-resumed",
        ConnectionPhase::Ready { resumed: false, .. } => "ready",
        ConnectionPhase::Closing { .. } => "closing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_never_render_attacker_controlled_values() {
        let marker = "alice@example.test/private-resource<credential/>";
        let parse_errors = [
            ParseError::InvalidXml(marker.to_string()),
            ParseError::UnknownRoot(marker.to_string()),
            ParseError::InvalidStanza {
                kind: "iq",
                err: marker.to_string(),
            },
        ];
        for category in parse_errors.iter().map(parse_error_category) {
            assert!(!category.contains(marker));
            assert!(category
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'-'));
        }

        let mechanism = SaslMechanism::from_wire_name(marker);
        assert_eq!(sasl_mechanism_category(&mechanism), "unsupported");

        let jid = "alice@example.test/private-resource"
            .parse()
            .expect("sensitive full JID");
        let phase = ConnectionPhase::ready(jid, false);
        let phase_category = connection_phase_category(&phase);
        assert_eq!(phase_category, "ready");
        assert!(!phase_category.contains("alice"));
    }

    #[test]
    fn auth_resume_and_binding_logs_have_no_raw_untrusted_fields() {
        for source in [
            include_str!("frame.rs"),
            include_str!("registration.rs"),
            include_str!("sasl.rs"),
            include_str!("isr_resume.rs"),
            include_str!("resource_binding.rs"),
            include_str!("stream_management.rs"),
        ] {
            for forbidden in [
                "error = %",
                "%error",
                "%err",
                "%mechanism",
                "phase = ?phase",
                "stream_id = %",
                "session = %",
                "jid = %",
                "claimed = %",
                "actual = %",
                "id = %",
                "raw = %",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "raw telemetry field {forbidden:?} reintroduced"
                );
            }
        }
    }
}
