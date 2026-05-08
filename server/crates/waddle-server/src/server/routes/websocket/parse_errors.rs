use super::transport_xml::sasl_failure_xml;
use super::*;

pub(super) fn is_sasl_parse_failure(frame: &str, err: &ParseError) -> bool {
    match (parse_error_root_name(frame), err) {
        (Some("auth" | "response"), ParseError::MalformedSasl(_) | ParseError::InvalidXml(_)) => {
            raw_xml_attr_value(frame, "xmlns").unwrap_or(waddle_xmpp::ns::SASL)
                == waddle_xmpp::ns::SASL
        }
        _ => false,
    }
}

pub(super) fn parse_error_responses(frame: &str, err: &ParseError) -> Option<Vec<String>> {
    match (parse_error_root_name(frame), err) {
        _ if is_sasl_parse_failure(frame, err) => Some(vec![sasl_failure_xml("malformed-request")]),
        (Some("iq"), ParseError::InvalidStanza { kind: "iq", .. } | ParseError::InvalidXml(_)) => {
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

fn invalid_iq_parse_error_response(frame: &str) -> Option<String> {
    let patched = inject_client_ns_if_missing(frame);
    let parsed = Element::from_str(&patched).ok();
    let namespace = parsed
        .as_ref()
        .map(|element| element.ns().to_string())
        .or_else(|| decoded_raw_xml_attr_value(frame, "xmlns"))
        .unwrap_or(waddle_xmpp::ns::JABBER_CLIENT.to_string());
    if namespace.as_str() != waddle_xmpp::ns::JABBER_CLIENT {
        return None;
    }
    let iq_type = parsed
        .as_ref()
        .and_then(|element| element.attr("type"))
        .map(ToString::to_string)
        .or_else(|| decoded_raw_xml_attr_value(frame, "type"))?;
    if matches!(iq_type.as_str(), "result" | "error") {
        return None;
    }

    let id = parsed
        .as_ref()
        .and_then(|element| element.attr("id"))
        .map(ToString::to_string)
        .or_else(|| decoded_raw_xml_attr_value(frame, "id"))
        .unwrap_or_default();
    let response_from = parsed
        .as_ref()
        .and_then(|element| element.attr("to"))
        .map(ToString::to_string)
        .or_else(|| decoded_raw_xml_attr_value(frame, "to"));
    let response_to = parsed
        .as_ref()
        .and_then(|element| element.attr("from"))
        .map(ToString::to_string)
        .or_else(|| decoded_raw_xml_attr_value(frame, "from"));
    Some(build_iq_error_xml_typed(
        &id,
        response_from.as_deref(),
        response_to.as_deref(),
        feature_not_implemented_iq_error("Requested feature not implemented."),
    ))
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
