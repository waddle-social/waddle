use crate::XmppError;

/// Parsed stream header information.
#[derive(Debug, Clone, Default)]
pub struct StreamHeader {
    /// The 'to' attribute (target domain)
    pub to: Option<String>,
    /// The 'from' attribute (source domain)
    pub from: Option<String>,
    /// The 'id' attribute (stream ID, set by server)
    pub id: Option<String>,
    /// The 'version' attribute (should be "1.0")
    pub version: Option<String>,
    /// The 'xml:lang' attribute
    pub lang: Option<String>,
}

impl StreamHeader {
    /// Parse a stream header from raw XML data.
    ///
    /// This handles the special case of XMPP stream headers which are
    /// incomplete XML (the closing tag comes at session end).
    pub fn parse(data: &str) -> Result<Self, XmppError> {
        let mut header = StreamHeader::default();

        // Find the stream:stream opening tag
        let stream_start = data
            .find("<stream:stream")
            .or_else(|| data.find("<stream "))
            .ok_or_else(|| XmppError::xml_parse("No stream:stream element found"))?;

        let stream_end = data[stream_start..]
            .find('>')
            .map(|i| stream_start + i)
            .ok_or_else(|| XmppError::xml_parse("Incomplete stream header"))?;

        let tag = &data[stream_start..=stream_end];

        // Parse attributes manually since the tag is intentionally unclosed
        header.to = extract_attribute(tag, "to");
        header.from = extract_attribute(tag, "from");
        header.id = extract_attribute(tag, "id");
        header.version = extract_attribute(tag, "version");
        header.lang = extract_attribute(tag, "xml:lang");

        Ok(header)
    }

    /// Validate the stream header per RFC 6120.
    pub fn validate(&self) -> Result<(), XmppError> {
        // Version should be 1.0 for RFC 6120
        if let Some(ref version) = self.version {
            if version != "1.0" {
                return Err(XmppError::stream(format!(
                    "Unsupported XMPP version: {}",
                    version
                )));
            }
        }
        Ok(())
    }
}

/// Extract an attribute value from an XML tag string.
pub(super) fn extract_attribute(tag: &str, name: &str) -> Option<String> {
    // Try both single and double quotes
    for quote in ['"', '\''] {
        let pattern = format!("{}={}", name, quote);
        if let Some(start) = tag.find(&pattern) {
            let value_start = start + pattern.len();
            if let Some(value_end) = tag[value_start..].find(quote) {
                return Some(tag[value_start..value_start + value_end].to_string());
            }
        }
    }
    None
}
