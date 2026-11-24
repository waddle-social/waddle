use super::handlers::iq::errors::{bad_request_iq_error, jid_malformed_iq_error};
use super::transport_xml::{element_to_xml, sasl2_failure_element, sasl_failure_xml};
use super::{build_iq_error_xml_typed, inject_client_ns_if_missing, Element, FromStr, ParseError};

pub(super) fn is_sasl_parse_failure(frame: &str, err: &ParseError) -> bool {
    if !matches!(
        err,
        ParseError::MalformedSasl(_) | ParseError::InvalidXml(_)
    ) {
        return false;
    }
    is_sasl2_parse_failure(frame, err)
        || match parse_error_root_name(frame) {
            Some("auth" | "response") => {
                raw_xml_attr_value(frame, "xmlns").unwrap_or(waddle_xmpp::ns::SASL)
                    == waddle_xmpp::ns::SASL
            }
            Some(root) => match root.split_once(':') {
                Some((prefix, "auth" | "response")) if !prefix.is_empty() => {
                    let namespace_attr = format!("xmlns:{prefix}");
                    decoded_raw_xml_attr_value(frame, &namespace_attr).as_deref()
                        == Some(waddle_xmpp::ns::SASL)
                }
                _ => false,
            },
            _ => false,
        }
}

pub(super) fn is_sasl2_parse_failure(frame: &str, err: &ParseError) -> bool {
    if !matches!(
        err,
        ParseError::MalformedSasl(_) | ParseError::InvalidXml(_)
    ) {
        return false;
    }
    match parse_error_root_name(frame) {
        Some("authenticate" | "response") => {
            raw_xml_attr_value(frame, "xmlns") == Some(waddle_xmpp_core::xep0388::NS_SASL2)
        }
        Some(root) => match root.split_once(':') {
            Some((prefix, "authenticate" | "response")) if !prefix.is_empty() => {
                let namespace_attr = format!("xmlns:{prefix}");
                decoded_raw_xml_attr_value(frame, &namespace_attr).as_deref()
                    == Some(waddle_xmpp_core::xep0388::NS_SASL2)
            }
            _ => false,
        },
        _ => false,
    }
}

pub(super) fn is_sasl2_response_parse_failure(frame: &str, err: &ParseError) -> bool {
    if !matches!(
        err,
        ParseError::MalformedSasl(_) | ParseError::InvalidXml(_)
    ) {
        return false;
    }
    match parse_error_root_name(frame) {
        Some("response") => {
            raw_xml_attr_value(frame, "xmlns") == Some(waddle_xmpp_core::xep0388::NS_SASL2)
        }
        Some(root) => match root.split_once(':') {
            Some((prefix, "response")) if !prefix.is_empty() => {
                let namespace_attr = format!("xmlns:{prefix}");
                decoded_raw_xml_attr_value(frame, &namespace_attr).as_deref()
                    == Some(waddle_xmpp_core::xep0388::NS_SASL2)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Whether a frame that parsing refused is nonetheless a stanza: a
/// well-formed `<iq/>`, `<message/>` or `<presence/>` in the `jabber:client`
/// namespace, refused for its content (an address outside the identity
/// profile, or a payload the typed parser rejects).
///
/// Such a frame counts toward the XEP-0198 handled count. A frame that is
/// not well-formed XML, an element in another namespace, and every
/// non-stanza element (SASL, stream management, an unknown root) are not
/// stanzas and are not counted.
pub(super) fn is_refused_client_stanza(frame: &str, err: &ParseError) -> bool {
    if !matches!(
        err,
        ParseError::IdentityProfile { .. } | ParseError::InvalidStanza { .. }
    ) {
        return false;
    }
    Element::from_str(&inject_client_ns_if_missing(frame)).is_ok_and(|element| {
        element.ns() == waddle_xmpp::ns::JABBER_CLIENT
            && matches!(element.name(), "iq" | "message" | "presence")
    })
}

pub(super) fn parse_error_responses(frame: &str, err: &ParseError) -> Option<Vec<String>> {
    match (parse_error_root_name(frame), err) {
        _ if is_sasl2_parse_failure(frame, err) => {
            Some(vec![element_to_xml(sasl2_failure_element(
                waddle_xmpp_core::xep0388::Sasl2FailureCondition::MalformedRequest,
            ))])
        }
        _ if is_sasl_parse_failure(frame, err) => Some(vec![sasl_failure_xml("malformed-request")]),
        (_, ParseError::IdentityProfile { kind, .. }) => {
            identity_profile_error_response(frame, kind).map(|response| vec![response])
        }
        (Some("iq"), ParseError::InvalidStanza { kind: "iq", .. }) => {
            invalid_iq_parse_error_response(frame).map(|response| vec![response])
        }
        _ => None,
    }
}

fn parse_error_root_name(frame: &str) -> Option<&str> {
    let trimmed = frame.trim_start();
    let rest = trimmed.strip_prefix('<')?;
    if rest.starts_with('?') || rest.starts_with('!') {
        return None;
    }
    let name_end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }
    Some(&rest[..name_end])
}

/// The `<bad-request/>` error (RFC 6120 §8.3.3.1) for an IQ get or set that
/// is well-formed XML but violates the IQ schema: the payload is missing,
/// repeated, or otherwise cannot be processed.
///
/// An IQ result or error is never answered (RFC 6120 §8.2.3). The error
/// keeps the request's `id`, and swaps `to` and `from` so it returns to the
/// sender.
fn invalid_iq_parse_error_response(frame: &str) -> Option<String> {
    let iq = Element::from_str(&inject_client_ns_if_missing(frame)).ok()?;
    if iq.ns() != waddle_xmpp::ns::JABBER_CLIENT {
        return None;
    }
    if !matches!(iq.attr("type"), Some("get" | "set")) {
        return None;
    }
    Some(build_iq_error_xml_typed(
        iq.attr("id").unwrap_or_default(),
        iq.attr("to"),
        iq.attr("from"),
        bad_request_iq_error("The IQ does not conform to the stanza schema."),
    ))
}

/// The `<jid-malformed/>` error (RFC 6120 §8.3.3.8) for a stanza refused
/// because an address in it is outside the identity profile.
///
/// The error goes back to the sending client without `from` or `to`, so it
/// echoes none of the refused addresses, and carries the stanza's `id`. An
/// error stanza, an IQ result and an IQ error are never answered (RFC 6120
/// §8.3.1 and §8.2.3).
fn identity_profile_error_response(frame: &str, kind: &'static str) -> Option<String> {
    let stanza = Element::from_str(&inject_client_ns_if_missing(frame)).ok()?;
    if stanza.ns() != waddle_xmpp::ns::JABBER_CLIENT {
        return None;
    }
    let stanza_type = stanza.attr("type");
    let answerable = match kind {
        "iq" => matches!(stanza_type, Some("get" | "set")),
        _ => stanza_type != Some("error"),
    };
    if !answerable {
        return None;
    }
    let error = jid_malformed_iq_error("An address in the stanza is not in canonical form.");
    let mut reply = Element::builder(kind, waddle_xmpp::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error");
    if let Some(id) = stanza.attr("id") {
        reply = reply.attr(minidom::rxml::xml_ncname!("id").to_owned(), id);
    }
    Some(element_to_xml(reply.append(Element::from(error)).build()))
}

fn decoded_raw_xml_attr_value(xml: &str, attr: &str) -> Option<String> {
    raw_xml_attr_value(xml, attr).map(decode_xml_attr_value)
}

fn decode_xml_attr_value(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }

    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(pos) = rest.find('&') {
        decoded.push_str(&rest[..pos]);
        let entity_start = &rest[pos + 1..];
        let Some(entity_end) = entity_start.find(';') else {
            decoded.push_str(&rest[pos..]);
            return decoded;
        };
        let entity = &entity_start[..entity_end];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|dec| dec.parse::<u32>().ok())
                })
                .and_then(char::from_u32)
                .filter(|&ch| is_valid_xml_char(ch)),
        };

        if let Some(ch) = replacement {
            decoded.push(ch);
        } else {
            decoded.push('&');
            decoded.push_str(entity);
            decoded.push(';');
        }
        rest = &entity_start[entity_end + 1..];
    }
    decoded.push_str(rest);
    decoded
}

fn is_valid_xml_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{9}'
            | '\u{A}'
            | '\u{D}'
            | '\u{20}'..='\u{D7FF}'
            | '\u{E000}'..='\u{FFFD}'
            | '\u{10000}'..='\u{10FFFF}'
    )
}

fn looks_like_attr_token(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    looks_like_attr_name(name)
}

fn looks_like_attr_name(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.'))
}

fn raw_xml_attr_value<'a>(xml: &'a str, attr: &str) -> Option<&'a str> {
    let trimmed = xml.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.first().copied()? != b'<' {
        return None;
    }

    let mut idx = 1;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    if idx >= bytes.len() || matches!(bytes[idx], b'/' | b'!' | b'?') {
        return None;
    }

    while idx < bytes.len()
        && !bytes[idx].is_ascii_whitespace()
        && !matches!(bytes[idx], b'/' | b'>')
    {
        idx += 1;
    }

    let mut fallback = None;
    while idx < bytes.len() {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() || matches!(bytes[idx], b'>' | b'/') {
            break;
        }

        let name_start = idx;
        while idx < bytes.len()
            && !bytes[idx].is_ascii_whitespace()
            && !matches!(bytes[idx], b'=' | b'>' | b'/')
        {
            idx += 1;
        }
        let name_end = idx;

        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() || bytes[idx] != b'=' {
            continue;
        }
        idx += 1;

        let value_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let had_value_whitespace = idx != value_start;
        let Some(&quote) = bytes.get(idx) else {
            return fallback;
        };
        if quote != b'"' && quote != b'\'' {
            let rest = &trimmed[idx..];
            let token_end = rest
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(rest.len());
            let token = &rest[..token_end];
            let looks_like_next_attr = had_value_whitespace
                && (looks_like_attr_token(token)
                    || (looks_like_attr_name(token)
                        && rest[token_end..].trim_start().starts_with('=')));
            if !looks_like_next_attr {
                if &trimmed[name_start..name_end] == attr {
                    fallback = Some(token.trim_end_matches('/'));
                }
                idx += token_end;
            }
            continue;
        }
        idx += 1;
        let value_start = idx;
        while idx < bytes.len() && bytes[idx] != quote {
            idx += 1;
        }
        if idx >= bytes.len() {
            return fallback;
        }

        if &trimmed[name_start..name_end] == attr {
            return Some(&trimmed[value_start..idx]);
        }

        idx += 1;
    }

    fallback
}
