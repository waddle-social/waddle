use crate::XmppError;

/// Parsed client-first-message components.
#[derive(Debug, Clone)]
pub(super) struct ClientFirstMessage {
    /// GS2 channel binding flag ('n', 'y', or 'p')
    pub(super) gs2_cbind_flag: char,
    /// Optional authzid, saslname-decoded. Authorization-identity
    /// mapping is not implemented, so the server only accepts an
    /// authzid naming the authenticating user itself.
    pub(super) authzid: Option<String>,
    /// The verbatim GS2 header (`[flag],[a=authzid],`) exactly as the
    /// client sent it; client-final `c=` must be its base64 encoding.
    pub(super) gs2_header: String,
    /// Username (authcid)
    pub(super) username: String,
    /// Client nonce
    pub(super) client_nonce: String,
    /// The bare message (without GS2 header) for auth message computation
    pub(super) bare: String,
}

/// Parsed client-final-message components.
#[derive(Debug, Clone)]
pub(super) struct ClientFinalMessage {
    /// Channel binding data (base64) - required by SCRAM protocol;
    /// verified against the GS2 header in `process_client_final`.
    pub(super) channel_binding: String,
    /// Combined nonce
    pub(super) nonce: String,
    /// Client proof (base64)
    pub(super) proof: String,
    /// Message without proof for auth message computation
    pub(super) without_proof: String,
}

/// Parse client-first-message.
///
/// Format: `gs2-header client-first-message-bare`
/// gs2-header: `[flag],authzid,`
/// client-first-message-bare: `n=username,r=nonce[,extensions]`
pub(super) fn parse_client_first(message: &str) -> Result<ClientFirstMessage, XmppError> {
    // Split by comma, but we need to handle the GS2 header specially
    let parts: Vec<&str> = message.splitn(3, ',').collect();
    if parts.len() < 3 {
        return Err(XmppError::auth_failed(
            "Invalid client-first-message format",
        ));
    }

    // Parse GS2 header. RFC 5802 ABNF: the flag token is exactly "n",
    // "y", or "p=<cb-name>" — a longer token like "nonsense" must not
    // pass as 'n'.
    let gs2_cbind_flag = match parts[0] {
        "n" => 'n',
        "y" => 'y',
        p if p.starts_with("p=") && p.len() > 2 => 'p',
        _ => {
            return Err(XmppError::auth_failed("Invalid GS2 channel binding flag"));
        }
    };

    // Parse optional authzid (a=...); RFC 5802 encodes it as a saslname.
    let authzid = if let Some(raw) = parts[1].strip_prefix("a=") {
        Some(decode_sasl_name(raw)?)
    } else if parts[1].is_empty() {
        None
    } else {
        return Err(XmppError::auth_failed("Invalid authzid format"));
    };

    let gs2_header = format!("{},{},", parts[0], parts[1]);

    // The rest is client-first-message-bare
    let bare = parts[2].to_string();

    // Parse the bare message for username and nonce
    let mut username = None;
    let mut client_nonce = None;

    for attr in bare.split(',') {
        if let Some(val) = attr.strip_prefix("n=") {
            // Decode username (RFC 5802 SASLprep and escaping)
            username = Some(decode_sasl_name(val)?);
        } else if let Some(val) = attr.strip_prefix("r=") {
            client_nonce = Some(val.to_string());
        }
        // Ignore other extensions
    }

    let username = username
        .ok_or_else(|| XmppError::auth_failed("Missing username in client-first-message"))?;
    let client_nonce = client_nonce
        .ok_or_else(|| XmppError::auth_failed("Missing nonce in client-first-message"))?;

    Ok(ClientFirstMessage {
        gs2_cbind_flag,
        authzid,
        gs2_header,
        username,
        client_nonce,
        bare,
    })
}

/// Parse client-final-message.
///
/// Format: `c=channel-binding,r=nonce,p=proof`
pub(super) fn parse_client_final(message: &str) -> Result<ClientFinalMessage, XmppError> {
    let mut channel_binding = None;
    let mut nonce = None;
    let mut proof = None;

    // Find the proof part to separate it
    let proof_idx = message
        .rfind(",p=")
        .ok_or_else(|| XmppError::auth_failed("Missing proof in client-final-message"))?;

    let without_proof = &message[..proof_idx];

    for attr in message.split(',') {
        if let Some(val) = attr.strip_prefix("c=") {
            channel_binding = Some(val.to_string());
        } else if let Some(val) = attr.strip_prefix("r=") {
            nonce = Some(val.to_string());
        } else if let Some(val) = attr.strip_prefix("p=") {
            proof = Some(val.to_string());
        }
    }

    let channel_binding = channel_binding
        .ok_or_else(|| XmppError::auth_failed("Missing channel binding in client-final-message"))?;
    let nonce =
        nonce.ok_or_else(|| XmppError::auth_failed("Missing nonce in client-final-message"))?;
    let proof =
        proof.ok_or_else(|| XmppError::auth_failed("Missing proof in client-final-message"))?;

    Ok(ClientFinalMessage {
        channel_binding,
        nonce,
        proof,
        without_proof: without_proof.to_string(),
    })
}

/// Decode a SASL name (RFC 5802 escaping).
/// - `=2C` -> `,`
/// - `=3D` -> `=`
pub(super) fn decode_sasl_name(name: &str) -> Result<String, XmppError> {
    let mut result = String::new();
    let mut chars = name.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '=' {
            let escape: String = chars.by_ref().take(2).collect();
            match escape.as_str() {
                "2C" => result.push(','),
                "3D" => result.push('='),
                _ => {
                    return Err(XmppError::auth_failed(format!(
                        "Invalid SASL name escape: ={}",
                        escape
                    )));
                }
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

/// Encode a SASL name (RFC 5802 escaping).
/// - `,` -> `=2C`
/// - `=` -> `=3D`
pub fn encode_sasl_name(name: &str) -> String {
    let mut result = String::new();
    for c in name.chars() {
        match c {
            ',' => result.push_str("=2C"),
            '=' => result.push_str("=3D"),
            _ => result.push(c),
        }
    }
    result
}
