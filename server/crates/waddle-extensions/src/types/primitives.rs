use super::*;

typed_non_empty_string!(ActionId, "action id");
typed_non_empty_string!(CommandSessionId, "command session id");
typed_non_empty_string!(DisplayText, "display text");
typed_non_empty_string!(EnrichmentId, "enrichment id");
typed_non_empty_string!(ListId, "list id");
typed_non_empty_string!(ListItemId, "list item id");
typed_non_empty_string!(LaunchId, "launch id");
typed_non_empty_string!(LaunchToken, "launch token");
typed_non_empty_string!(MediaType, "media type");
typed_non_empty_string!(PluginVersion, "plugin version");
typed_non_empty_string!(ProviderDeliveryId, "provider delivery id");
typed_non_empty_string!(ProviderEventType, "provider event type");
typed_non_empty_string!(ProviderFieldName, "provider field name");
typed_non_empty_string!(ProviderFieldNumber, "provider field number");
typed_non_empty_string!(ProviderId, "provider id");
typed_non_empty_string!(PubSubItemId, "pubsub item id");
typed_non_empty_string!(PubSubNode, "pubsub node");
typed_non_empty_string!(RouteId, "route id");
typed_non_empty_string!(StanzaId, "stanza id");
typed_non_empty_string!(Timestamp, "timestamp");
typed_non_empty_string!(ThreadId, "thread id");
typed_non_empty_string!(UiActionId, "ui action id");
typed_non_empty_string!(UiViewId, "ui view id");
typed_non_empty_string!(Url, "url");
typed_non_empty_string!(WaddleId, "waddle id");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderFieldText(String);

impl ProviderFieldText {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomJid(String);

impl RoomJid {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        value
            .parse::<BareJid>()
            .map_err(|_| FrameworkTypeError::InvalidBareJid(value.clone()))?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FullJidValue(String);

impl FullJidValue {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        value
            .parse::<FullJid>()
            .map_err(|_| FrameworkTypeError::InvalidFullJid(value.clone()))?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginId(String);

impl PluginId {
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

    pub fn as_str(&self) -> &str {
        &self.0
    }

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PayloadNamespace(String);

impl PayloadNamespace {
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

    pub fn framework() -> Self {
        Self(FRAMEWORK_NAMESPACE.to_string())
    }

    pub fn is_framework(&self) -> bool {
        self.0 == FRAMEWORK_NAMESPACE
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandNode(String);

impl CommandNode {
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

    pub fn invoke() -> Self {
        Self(INVOKE_COMMAND_NODE.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for CommandNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
        let value = value.into();
        if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(FrameworkTypeError::InvalidSha256Digest(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactUri(String);

impl ArtifactUri {
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

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactReference {
    pub uri: ArtifactUri,
    pub sha256: Sha256Digest,
    pub media_type: Option<MediaType>,
}

impl ArtifactReference {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BodyRange {
    pub start: u32,
    pub end: u32,
}

impl BodyRange {
    pub fn new(start: u32, end: u32) -> Result<Self, FrameworkTypeError> {
        if end > start {
            Ok(Self { start, end })
        } else {
            Err(FrameworkTypeError::InvalidBodyRange { start, end })
        }
    }
}
