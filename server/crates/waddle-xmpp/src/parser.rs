//! Incremental XML parsing for XMPP streams using rxml and minidom.
//!
//! XMPP uses a single long-lived XML document per session, so we need
//! incremental parsing that can handle partial data and maintain state
//! across multiple read operations.

use minidom::Element;
use std::collections::VecDeque;

use crate::stream_management::SmStanza;
use crate::XmppError;

mod serialization;
mod stream_header;

pub mod ns;

pub use serialization::{element_to_string, message_to_string, stanza_to_string};
use stream_header::extract_attribute;
pub use stream_header::StreamHeader;

/// Incremental XML parser for XMPP stanzas.
///
/// This parser accumulates data and emits complete XML elements (stanzas)
/// as they become available. It handles the XMPP stream framing where
/// the stream:stream element wraps all stanzas.
pub struct XmlParser {
    /// Accumulated data buffer
    buffer: Vec<u8>,
    /// Queue of parsed elements ready to be consumed
    elements: VecDeque<Element>,
    /// Whether we've seen the stream header
    stream_started: bool,
    /// Current parsing depth (for tracking element boundaries)
    depth: usize,
    /// Start position of current element
    element_start: Option<usize>,
}

impl XmlParser {
    /// Create a new XML parser.
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(8192),
            elements: VecDeque::new(),
            stream_started: false,
            depth: 0,
            element_start: None,
        }
    }

    /// Feed data into the parser.
    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Check if we have a complete stream header in the buffer.
    pub fn has_stream_header(&self) -> bool {
        let s = String::from_utf8_lossy(&self.buffer);
        (s.contains("<stream:stream") || s.contains("<stream ")) && s.contains('>')
    }

    /// Extract and consume the stream header from the buffer.
    pub fn take_stream_header(&mut self) -> Result<StreamHeader, XmppError> {
        let data = String::from_utf8_lossy(&self.buffer).to_string();
        let header = StreamHeader::parse(&data)?;
        self.stream_started = true;
        // Don't clear buffer - there might be more data after the header
        Ok(header)
    }

    /// Check if there's a complete stanza in the buffer.
    ///
    /// Returns true if we have a complete top-level element after the stream header.
    pub fn has_complete_stanza(&self) -> bool {
        let data = String::from_utf8_lossy(&self.buffer);

        // Check for stream close
        if data.contains("</stream:stream>") {
            return true;
        }

        // Simple heuristic: look for matching opening and closing tags
        // for top-level WebSocket-served stanzas and nonzas.
        let stanza_tags = [
            "message",
            "presence",
            "iq",
            "auth",
            "success",
            "stream:features",
            // XEP-0198 Stream Management elements
            "enable",
            "enabled",
            "resume",
            "resumed",
            "failed",
            "r",
            "a",
        ];

        for tag in stanza_tags {
            if let Some(start) = data.find(&format!("<{}", tag)) {
                // Check for self-closing tag
                let after_tag = &data[start..];
                if let Some(end) = after_tag.find('>') {
                    if end > 0 && after_tag.as_bytes()[end - 1] == b'/' {
                        return true;
                    }
                }
                // Check for closing tag
                if data.contains(&format!("</{}", tag)) {
                    return true;
                }
            }
        }

        false
    }

    /// Parse and return the next complete stanza from the buffer.
    ///
    /// Returns None if no complete stanza is available.
    pub fn next_stanza(&mut self) -> Result<Option<ParsedStanza>, XmppError> {
        let data = String::from_utf8_lossy(&self.buffer).to_string();

        // Check for stream close
        if data.contains("</stream:stream>") {
            if let Some(pos) = data.find("</stream:stream>") {
                self.buffer = self.buffer[pos + 16..].to_vec();
            }
            return Ok(Some(ParsedStanza::StreamEnd));
        }

        // Try to parse each known stanza type
        // NOTE: Pattern order matters! More specific patterns must come before less specific ones.
        // e.g., "<resume" must come before "<r" since "<r" would otherwise match "<resume"
        type StanzaParser = fn(&str) -> Result<ParsedStanza, XmppError>;
        let stanza_patterns: &[(&str, StanzaParser)] = &[
            ("<stream:features", parse_stream_features),
            ("<stream:error", parse_stream_error),
            ("<auth", parse_auth),
            ("<response", parse_sasl_response), // SASL response for SCRAM
            ("<iq", parse_iq_stanza),
            ("<message", parse_message_stanza),
            ("<presence", parse_presence_stanza),
            // XEP-0198 Stream Management stanzas. All routes funnel through
            // `parse_sm_nonza` (which delegates to stream_management::SmStanza)
            // so stream-oriented callers share one parsing path.
            ("<enable", parse_sm_nonza),
            ("<resume", parse_sm_nonza), // Must come before <r
            ("<r", parse_sm_nonza),
            ("<a ", parse_sm_nonza), // Note: space to avoid matching <auth
        ];

        // Find the earliest matching pattern in the buffer.
        // Ties (same start position) are broken by pattern list order,
        // which ensures e.g. "<resume" is preferred over "<r".
        let mut best: Option<(usize, &str, StanzaParser)> = None;
        for (pattern, parser) in stanza_patterns {
            if let Some(start) = data.find(pattern) {
                if best.is_none() || start < best.unwrap().0 {
                    best = Some((start, pattern, *parser));
                }
            }
        }

        if let Some((start, pattern, parser)) = best {
            let tag_name = &pattern[1..].trim(); // Strip leading < and any trailing space
            if let Some(end) = find_stanza_end(&data, start, tag_name) {
                let stanza_xml = &data[start..end];
                let result = parser(stanza_xml)?;

                // Remove parsed data from buffer
                self.buffer = data.as_bytes()[end..].to_vec();
                return Ok(Some(result));
            }
        }

        Ok(None)
    }

    /// Clear the parser state and buffer.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.elements.clear();
        self.stream_started = false;
        self.depth = 0;
        self.element_start = None;
    }

    /// Get the current buffer contents as a string (for debugging).
    pub fn buffer_str(&self) -> String {
        String::from_utf8_lossy(&self.buffer).to_string()
    }
}

impl Default for XmlParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Find the end position of a stanza (after the closing tag or self-closing tag).
fn find_stanza_end(data: &str, start: usize, tag_name: &str) -> Option<usize> {
    let after_start = &data[start..];

    // Check for self-closing tag first
    if let Some(gt_pos) = after_start.find('>') {
        if gt_pos > 0 && after_start.as_bytes()[gt_pos - 1] == b'/' {
            return Some(start + gt_pos + 1);
        }
    }

    // Look for closing tag
    let close_tag = format!("</{}", tag_name);
    if let Some(close_start) = after_start.find(&close_tag) {
        // Find the > after the closing tag
        if let Some(close_end) = after_start[close_start..].find('>') {
            return Some(start + close_start + close_end + 1);
        }
    }

    None
}

/// Parsed stanza variants.
#[derive(Debug, Clone)]
pub enum ParsedStanza {
    /// Stream features
    Features {
        /// SASL mechanisms available
        sasl_mechanisms: Vec<String>,
    },
    /// Stream error
    StreamError {
        /// Error condition
        condition: String,
        /// Optional error text
        text: Option<String>,
    },
    /// SASL auth request with mechanism and base64 data
    SaslAuth { mechanism: String, data: String },
    /// SASL response (for multi-step auth like SCRAM) with base64 data
    SaslResponse { data: String },
    /// Stream end
    StreamEnd,
    /// Message stanza
    Message(Element),
    /// Presence stanza
    Presence(Element),
    /// IQ stanza
    Iq(Element),
    /// Unknown/raw element
    Unknown(Element),
    /// XEP-0198: Stream Management enable request
    SmEnable { resume: bool, max: Option<u32> },
    /// XEP-0198: Stream Management request ack
    SmRequest,
    /// XEP-0198: Stream Management ack response
    SmAck { h: u32 },
    /// XEP-0198: Stream Management resume request
    SmResume { previd: String, h: u32 },
}

/// Parse stream features element.
fn parse_stream_features(data: &str) -> Result<ParsedStanza, XmppError> {
    // Extract SASL mechanisms
    let mut sasl_mechanisms = Vec::new();
    let mut search_pos = 0;
    while let Some(start) = data[search_pos..].find("<mechanism>") {
        let actual_start = search_pos + start + 11; // "<mechanism>".len()
        if let Some(end) = data[actual_start..].find("</mechanism>") {
            let mechanism = data[actual_start..actual_start + end].trim();
            sasl_mechanisms.push(mechanism.to_string());
            search_pos = actual_start + end;
        } else {
            break;
        }
    }

    Ok(ParsedStanza::Features { sasl_mechanisms })
}

/// Parse stream error element.
fn parse_stream_error(data: &str) -> Result<ParsedStanza, XmppError> {
    // Common stream error conditions
    let conditions = [
        "bad-format",
        "bad-namespace-prefix",
        "conflict",
        "connection-timeout",
        "host-gone",
        "host-unknown",
        "improper-addressing",
        "internal-server-error",
        "invalid-from",
        "invalid-namespace",
        "invalid-xml",
        "not-authorized",
        "not-well-formed",
        "policy-violation",
        "remote-connection-failed",
        "reset",
        "resource-constraint",
        "restricted-xml",
        "see-other-host",
        "system-shutdown",
        "undefined-condition",
        "unsupported-encoding",
        "unsupported-feature",
        "unsupported-stanza-type",
        "unsupported-version",
    ];

    let condition = conditions
        .iter()
        .find(|c| data.contains(*c))
        .map(|c| c.to_string())
        .unwrap_or_else(|| "undefined-condition".to_string());

    // Try to extract text element
    let text = if let Some(start) = data.find("<text") {
        if let Some(content_start) = data[start..].find('>') {
            let actual_start = start + content_start + 1;
            data[actual_start..]
                .find("</text>")
                .map(|end| data[actual_start..actual_start + end].trim().to_string())
        } else {
            None
        }
    } else {
        None
    };

    Ok(ParsedStanza::StreamError { condition, text })
}

fn parse_auth(data: &str) -> Result<ParsedStanza, XmppError> {
    let mechanism = extract_attribute(data, "mechanism").unwrap_or_default();

    // Extract content between > and </auth>
    let content_start = data.find('>').map(|i| i + 1).unwrap_or(0);
    let content_end = data.find("</auth>").unwrap_or(data.len());
    let content = if content_start < content_end {
        data[content_start..content_end].trim().to_string()
    } else {
        String::new()
    };

    Ok(ParsedStanza::SaslAuth {
        mechanism,
        data: content,
    })
}

/// Parse SASL response stanza (for multi-step auth like SCRAM).
fn parse_sasl_response(data: &str) -> Result<ParsedStanza, XmppError> {
    // Extract content between > and </response>
    let content_start = data.find('>').map(|i| i + 1).unwrap_or(0);
    let content_end = data.find("</response>").unwrap_or(data.len());
    let content = if content_start < content_end {
        data[content_start..content_end].trim().to_string()
    } else {
        String::new()
    };

    Ok(ParsedStanza::SaslResponse { data: content })
}

fn parse_iq_stanza(data: &str) -> Result<ParsedStanza, XmppError> {
    let element = parse_client_element(data)?;
    Ok(ParsedStanza::Iq(element))
}

fn parse_message_stanza(data: &str) -> Result<ParsedStanza, XmppError> {
    let element = parse_client_element(data)?;
    Ok(ParsedStanza::Message(element))
}

fn parse_presence_stanza(data: &str) -> Result<ParsedStanza, XmppError> {
    let element = parse_client_element(data)?;
    Ok(ParsedStanza::Presence(element))
}

/// Parse XEP-0198 Stream Management nonzas through the shared parser in
/// `stream_management::SmStanza`.
fn parse_sm_nonza(data: &str) -> Result<ParsedStanza, XmppError> {
    let sm = SmStanza::parse(data).ok_or_else(|| XmppError::xml_parse("Invalid SM stanza"))?;

    match sm {
        SmStanza::Enable(enable) => Ok(ParsedStanza::SmEnable {
            resume: enable.resume,
            max: enable.max,
        }),
        SmStanza::Request => Ok(ParsedStanza::SmRequest),
        SmStanza::Ack(ack) => Ok(ParsedStanza::SmAck { h: ack.h }),
        SmStanza::Resume(resume) => Ok(ParsedStanza::SmResume {
            previd: resume.previd,
            h: resume.h,
        }),
        SmStanza::Enabled(_) | SmStanza::Resumed(_) | SmStanza::Failed(_) => Err(
            XmppError::xml_parse("Invalid SM client stanza: server-origin nonza"),
        ),
    }
}

/// Parse a string into a minidom Element.
#[cfg(test)]
fn parse_element(data: &str) -> Result<Element, XmppError> {
    data.parse::<Element>()
        .map_err(|e| XmppError::xml_parse(format!("Failed to parse element: {}", e)))
}

/// Parse a client stanza, tolerating omitted default namespace declarations.
///
/// Some clients rely on stream-level namespace inheritance and send top-level
/// `<iq/>`, `<message/>`, or `<presence/>` without `xmlns='jabber:client'`.
/// Since we parse stanzas as standalone fragments, we inject the default ns.
fn parse_client_element(data: &str) -> Result<Element, XmppError> {
    let patched = add_default_namespace_if_missing(data, ns::JABBER_CLIENT);
    patched
        .parse::<Element>()
        .map_err(|e| XmppError::xml_parse(format!("Failed to parse element: {}", e)))
}

fn add_default_namespace_if_missing(data: &str, default_ns: &str) -> String {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }

    if !(trimmed.starts_with("<iq")
        || trimmed.starts_with("<message")
        || trimmed.starts_with("<presence"))
    {
        return trimmed.to_string();
    }

    let open_end = match trimmed.find('>') {
        Some(idx) => idx,
        None => return trimmed.to_string(),
    };
    let open_tag = &trimmed[..open_end];

    if open_tag.contains("xmlns=") {
        return trimmed.to_string();
    }

    let tag_end = trimmed[1..]
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .map(|idx| idx + 1)
        .unwrap_or(open_end);

    format!(
        "{} xmlns='{}'{}",
        &trimmed[..tag_end],
        default_ns,
        &trimmed[tag_end..]
    )
}

#[cfg(test)]
mod tests;
