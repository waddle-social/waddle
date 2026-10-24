//! Validated identifiers, JIDs, namespaces, artifact references, and body ranges.

use super::*;

typed_non_empty_string!(
    ActionId,
    "action id",
    "Plugin-defined action selected by an authorized launch."
);
typed_non_empty_string!(
    CommandSessionId,
    "command session id",
    "Opaque session ID correlating successive stages of an ad-hoc command."
);
typed_non_empty_string!(
    DisplayText,
    "display text",
    "Nonblank human-readable text for host-rendered views and diagnostics."
);
typed_non_empty_string!(
    EnrichmentId,
    "enrichment id",
    "Plugin-defined identity of one message enrichment."
);
typed_non_empty_string!(
    ListId,
    "list id",
    "Identity of a declarative list within an extension view."
);
typed_non_empty_string!(
    ListItemId,
    "list item id",
    "Identity of one item within a declarative list."
);
typed_non_empty_string!(
    LaunchId,
    "launch id",
    "Identity linking a launch descriptor to its later invocation."
);
typed_non_empty_string!(
    LaunchToken,
    "launch token",
    "Opaque authorization token attached to a host-signed launch descriptor."
);
typed_non_empty_string!(
    MediaType,
    "media type",
    "Nonblank media-type text supplied with an artifact; MIME syntax is not parsed."
);
typed_non_empty_string!(
    PluginVersion,
    "plugin version",
    "Nonblank guest version text; semantic-version syntax is not required."
);
typed_non_empty_string!(
    ProviderDeliveryId,
    "provider delivery id",
    "Provider-assigned identity used to correlate a webhook delivery."
);
typed_non_empty_string!(
    ProviderEventType,
    "provider event type",
    "Provider-specific event category supplied to webhook dispatch."
);
typed_non_empty_string!(
    ProviderFieldName,
    "provider field name",
    "One named segment of a flattened provider payload path."
);
typed_non_empty_string!(
    ProviderFieldNumber,
    "provider field number",
    "Nonblank numeric text retained from a provider without parsing its numeric syntax."
);
typed_non_empty_string!(
    ProviderId,
    "provider id",
    "Identity of an external provider producing webhook events or usage records."
);
typed_non_empty_string!(
    PubSubItemId,
    "pubsub item id",
    "Nonblank PubSub item identity used for publication and lookup."
);
typed_non_empty_string!(
    PubSubNode,
    "pubsub node",
    "PubSub node name or manifest node pattern; node syntax is not otherwise parsed."
);
typed_non_empty_string!(
    RouteId,
    "route id",
    "Identity of a discoverable route within its owning extension."
);
typed_non_empty_string!(
    StanzaId,
    "stanza id",
    "Nonblank XMPP stanza identity used to correlate messages and extension output."
);
typed_non_empty_string!(
    OriginId,
    "origin id",
    "Sender-assigned XEP-0359 origin identity retained for durable observation correlation."
);
typed_non_empty_string!(
    ModelId,
    "model id",
    "Provider-specific model identity attached to invocation usage accounting."
);
typed_non_empty_string!(
    Timestamp,
    "timestamp",
    "Nonblank timestamp text; RFC 3339 parsing occurs only where an operation requires it."
);
typed_non_empty_string!(
    ThreadId,
    "thread id",
    "Conversation thread identity supplied with message or archive context."
);
typed_non_empty_string!(
    UiActionId,
    "ui action id",
    "Machine-readable field or action name used by extension forms."
);
typed_non_empty_string!(
    UiViewId,
    "ui view id",
    "Identity of a declarative view within an extension response."
);
typed_non_empty_string!(
    Url,
    "url",
    "Nonblank resource locator text; URL syntax and scheme are not parsed by this wrapper."
);
typed_non_empty_string!(
    WaddleId,
    "waddle id",
    "Community identity that scopes an extension invocation."
);

/// Provider text retained verbatim, including empty strings and whitespace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderFieldText(String);

impl ProviderFieldText {
    /// Wrap provider text verbatim, including empty strings and surrounding whitespace.
    ///
    /// # Errors
    ///
    /// This constructor currently accepts all text and never returns an error.
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        Ok(Self(value.into()))
    }

    /// Borrow the stored text without allocating or changing whitespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this wrapper and return its original owned text without copying.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for ProviderFieldText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ProviderFieldText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Text validated as a bare room JID, retaining its original spelling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomJid(String);

impl RoomJid {
    /// Validate that the input parses as a bare JID, retaining its original spelling.
    ///
    /// # Errors
    ///
    /// Returns `InvalidBareJid` if the text does not parse as a bare JID.
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        value
            .parse::<BareJid>()
            .map_err(|_| FrameworkTypeError::InvalidBareJid(value.clone()))?;
        Ok(Self(value))
    }

    /// Borrow the stored text without allocating or changing whitespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this wrapper and return its original owned text without copying.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for RoomJid {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for RoomJid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Text validated as a full JID with a resource, retaining its original spelling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FullJidValue(String);

impl FullJidValue {
    /// Validate that the input parses as a full JID, retaining its original spelling.
    ///
    /// # Errors
    ///
    /// Returns `InvalidFullJid` if the text does not parse as a full JID.
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        value
            .parse::<FullJid>()
            .map_err(|_| FrameworkTypeError::InvalidFullJid(value.clone()))?;
        Ok(Self(value))
    }

    /// Borrow the stored text without allocating or changing whitespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this wrapper and return its original owned text without copying.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for FullJidValue {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for FullJidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lowercase ASCII extension identifier with alphanumeric ends and optional interior hyphens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginId(String);

impl PluginId {
    /// Validate a lowercase ASCII plugin identifier with alphanumeric ends and optional
    /// interior hyphens.
    ///
    /// # Errors
    ///
    /// Returns `Empty` for empty input, or `InvalidPluginId` for characters or end
    /// positions outside the lowercase ASCII identifier grammar.
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        if value.is_empty() {
            return Err(FrameworkTypeError::Empty { field: "plugin id" });
        }
        let valid = value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && value
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            && value
                .bytes()
                .last()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if valid {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::InvalidPluginId(value))
        }
    }

    /// Borrow the stored text without allocating or changing whitespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this wrapper and return its original owned text without copying.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for PluginId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Non-official namespace accepted by prefix checks for `urn:` or `https://`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PayloadNamespace(String);

impl PayloadNamespace {
    /// Accept an `urn:` or `https://` namespace outside the reserved XMPP prefixes. This
    /// checks prefixes, not full URI syntax.
    ///
    /// # Errors
    ///
    /// Returns `OfficialNamespace` for reserved XMPP prefixes, or
    /// `InvalidPayloadNamespace` unless the value starts with `urn:` or `https://`.
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        if is_official_namespace(&value) {
            return Err(FrameworkTypeError::OfficialNamespace(value));
        }
        if value.starts_with("urn:") || value.starts_with("https://") {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::InvalidPayloadNamespace(value))
        }
    }

    /// Return the namespace used by Waddle's extension envelope and UI vocabulary.
    pub fn framework() -> Self {
        Self(FRAMEWORK_NAMESPACE.to_string())
    }

    /// Whether this namespace is exactly the Waddle extension framework namespace.
    pub fn is_framework(&self) -> bool {
        self.0 == FRAMEWORK_NAMESPACE
    }

    /// Borrow the stored text without allocating or changing whitespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this wrapper and return its original owned text without copying.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for PayloadNamespace {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PayloadNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ad-hoc command node inside the Waddle extension framework's namespace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandNode(String);

impl CommandNode {
    /// Accept the framework namespace itself or a command node beginning with that
    /// namespace followed by a colon.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCommandNode` unless the value is the framework namespace or starts
    /// with that namespace followed by a colon.
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        let valid = value == FRAMEWORK_NAMESPACE
            || value
                .strip_prefix(FRAMEWORK_NAMESPACE)
                .is_some_and(|suffix| suffix.starts_with(':'));
        if valid {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::InvalidCommandNode(value))
        }
    }

    /// Return the framework's generic launch-invocation ad-hoc command node.
    pub fn invoke() -> Self {
        Self(INVOKE_COMMAND_NODE.to_string())
    }

    /// Borrow the stored text without allocating or changing whitespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this wrapper and return its original owned text without copying.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for CommandNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Validated 64-character hexadecimal SHA-256 digest normalized to lowercase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    /// Validate 64 ASCII hexadecimal characters and normalize them to lowercase.
    ///
    /// # Errors
    ///
    /// Returns `InvalidSha256Digest` unless the value is exactly 64 ASCII hexadecimal characters.
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(FrameworkTypeError::InvalidSha256Digest(value))
        }
    }

    /// Borrow the stored text without allocating or changing whitespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// HTTP(S)-prefixed artifact locator containing the immutable `/sha256/` path convention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactUri(String);

impl ArtifactUri {
    /// Accept an HTTP(S)-prefixed artifact locator containing `/sha256/`; this validates
    /// the immutable-path convention rather than full URL syntax.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArtifactUri` unless the value starts with `http://` or `https://`
    /// and contains `/sha256/`.
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        let immutable_http = (value.starts_with("https://") || value.starts_with("http://"))
            && value.contains("/sha256/");
        if immutable_http {
            Ok(Self(value))
        } else {
            Err(FrameworkTypeError::InvalidArtifactUri(value))
        }
    }

    /// Borrow the stored text without allocating or changing whitespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Immutable artifact locator, matching SHA-256 digest, and optional content type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactReference {
    /// URI identifying the referenced resource.
    pub uri: ArtifactUri,
    /// Hexadecimal SHA-256 digest used to verify artifact content.
    pub sha256: Sha256Digest,
    /// MIME media type of the referenced content.
    pub media_type: Option<MediaType>,
}

impl ArtifactReference {
    /// Validate the artifact URI and digest, requiring the URI to contain the digest
    /// case-insensitively.
    ///
    /// # Errors
    ///
    /// Returns the URI or digest validation error, or `ArtifactDigestMismatch` when the
    /// URI does not contain the normalized digest.
    pub fn new(
        uri: impl Into<String>,
        sha256: impl Into<String>,
        media_type: Option<MediaType>,
    ) -> Result<Self, FrameworkTypeError> {
        let uri = ArtifactUri::new(uri)?;
        let sha256 = Sha256Digest::new(sha256)?;
        if !uri.as_str().to_ascii_lowercase().contains(sha256.as_str()) {
            return Err(FrameworkTypeError::ArtifactDigestMismatch {
                uri: uri.as_str().to_string(),
                sha256: sha256.as_str().to_string(),
            });
        }
        Ok(Self {
            uri,
            sha256,
            media_type,
        })
    }

    pub(super) fn add_attrs(&self, builder: minidom::ElementBuilder) -> minidom::ElementBuilder {
        let mut builder = builder
            .attr(
                minidom::rxml::xml_ncname!("artifact-uri").to_owned(),
                self.uri.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("artifact-sha256").to_owned(),
                self.sha256.as_str(),
            );
        if let Some(media_type) = &self.media_type {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("media-type").to_owned(),
                media_type.as_str(),
            );
        }
        builder
    }
}

/// Nonempty half-open offset range in a message body; the containing API determines offset units.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BodyRange {
    /// Inclusive offset where the referenced body range begins.
    pub start: u32,
    /// Exclusive offset where the referenced body range ends.
    pub end: u32,
}

impl BodyRange {
    /// Create a nonempty half-open body range whose end is strictly greater than its start.
    ///
    /// # Errors
    ///
    /// Returns `InvalidBodyRange` when `end` is not strictly greater than `start`.
    pub fn new(start: u32, end: u32) -> Result<Self, FrameworkTypeError> {
        if end > start {
            Ok(Self { start, end })
        } else {
            Err(FrameworkTypeError::InvalidBodyRange { start, end })
        }
    }
}
