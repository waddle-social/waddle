use jid::BareJid;

use crate::pubsub::node::NodeConfig;
use crate::pubsub::stanzas::PubSubItem;

/// Stored representation of a PubSub node.
#[derive(Debug, Clone)]
pub struct PubSubNode {
    /// Unique node identifier (within an owner's namespace).
    pub node_name: String,
    /// The bare JID of the node owner.
    pub owner: BareJid,
    /// Node configuration.
    pub config: NodeConfig,
    /// When the node was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl PubSubNode {
    /// Create a new PubSub node with default PEP configuration.
    pub fn new_pep(owner: BareJid, node_name: String) -> Self {
        let config = NodeConfig::pep_for_node(&node_name);
        Self {
            node_name,
            owner,
            config,
            created_at: chrono::Utc::now(),
        }
    }

    /// Create a new PubSub node with custom configuration.
    pub fn new(owner: BareJid, node_name: String, config: NodeConfig) -> Self {
        Self {
            node_name,
            owner,
            config,
            created_at: chrono::Utc::now(),
        }
    }
}

/// Stored representation of a PubSub item.
#[derive(Debug, Clone)]
pub struct StoredItem {
    /// Item ID.
    pub id: String,
    /// The item payload as XML string.
    pub payload_xml: Option<String>,
    /// Publisher's JID.
    pub publisher: Option<BareJid>,
    /// When the item was published.
    pub published_at: chrono::DateTime<chrono::Utc>,
}

impl StoredItem {
    /// Convert to a PubSubItem for responses.
    pub fn to_pubsub_item(&self) -> PubSubItem {
        let payload = self.payload_xml.as_ref().and_then(|xml| xml.parse().ok());

        PubSubItem {
            id: Some(self.id.clone()),
            publisher: self.publisher.clone(),
            payload,
        }
    }
}

/// Result of a publish operation.
#[derive(Debug)]
pub struct PublishResult {
    /// The assigned item ID (may be generated if not provided).
    pub item_id: String,
    /// Whether a new node was created (auto-create).
    pub node_created: bool,
    /// Item IDs evicted by the node's `max_items` retention policy.
    pub evicted_item_ids: Vec<String>,
}

/// A host-selected backing node for XEP-0357 notifications.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublicationNode(String);

impl PublicationNode {
    pub fn new(value: impl Into<String>) -> Result<Self, PublicationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PublicationError::InvalidPublication);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Ordering of the PubSub projection, not a canonical delivery receipt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicationVersion {
    revision: u64,
    job_token: uuid::Uuid,
}

impl PublicationVersion {
    pub fn new(revision: u64, job_token: uuid::Uuid) -> Result<Self, PublicationError> {
        if revision == 0 || revision > i64::MAX as u64 || job_token.is_nil() {
            return Err(PublicationError::InvalidPublication);
        }
        Ok(Self {
            revision,
            job_token,
        })
    }

    pub fn revision(self) -> u64 {
        self.revision
    }
    pub fn job_token(self) -> uuid::Uuid {
        self.job_token
    }
}

impl std::fmt::Debug for PublicationVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicationVersion")
            .field("revision", &self.revision)
            .field("job_token", &"[redacted]")
            .finish()
    }
}

/// Integrity evidence for the approved item, never a delivery authority or a
/// retained notification body. Only storage code serializes this fingerprint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicationFingerprint([u8; 32]);

impl PublicationFingerprint {
    pub fn of(item: &PubSubItem, publisher: &BareJid) -> Result<Self, PublicationError> {
        use sha2::{Digest, Sha256};
        validate_push_publication(item)?;
        let id = item
            .id
            .as_ref()
            .ok_or(PublicationError::InvalidPublication)?;
        let payload = item
            .payload
            .as_ref()
            .ok_or(PublicationError::InvalidPublication)?;
        let mut hash = Sha256::new();
        hash.update(b"waddle.pubsub.push_projection.v1");
        for field in [id.clone(), publisher.to_string(), String::from(payload)] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        Ok(Self(hash.finalize().into()))
    }

    pub fn to_storage_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for PublicationFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PublicationFingerprint(..)")
    }
}

#[derive(Debug)]
pub enum VersionedPublishResult {
    Applied(PublishResult),
    AlreadyApplied,
    Superseded,
}

#[derive(Debug, thiserror::Error)]
pub enum PublicationError {
    #[error("invalid versioned push publication")]
    InvalidPublication,
    #[error("versioned push publication integrity conflict")]
    IntegrityConflict,
    #[error("versioned push publication storage failure: {0}")]
    Storage(#[from] crate::XmppError),
}

impl PublicationError {
    pub fn into_xmpp_error(self) -> crate::XmppError {
        match self {
            Self::Storage(error) => error,
            Self::InvalidPublication => {
                crate::XmppError::bad_request(Some("invalid versioned push publication".to_owned()))
            }
            Self::IntegrityConflict => {
                crate::XmppError::internal("versioned push publication integrity conflict")
            }
        }
    }
}

/// Validate the narrow host-only carrier before either storage backend writes.
/// No guest-supplied receipt or protocol-control namespace is introduced.
pub(super) fn validate_push_publication(item: &PubSubItem) -> Result<(), PublicationError> {
    let Some(id) = item.id.as_ref() else {
        return Err(PublicationError::InvalidPublication);
    };
    let Some(payload) = item.payload.as_ref() else {
        return Err(PublicationError::InvalidPublication);
    };
    if id.trim().is_empty()
        || payload.name() != "notification"
        || payload.ns() != crate::xep::xep0357::NS_PUSH
    {
        return Err(PublicationError::InvalidPublication);
    }
    Ok(())
}
