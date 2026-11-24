//! Publication to remote XMPP Push Services (XEP-0357 §5): a registration
//! whose Push Service is another server's, such as the app server of a
//! third-party client, is published to over S2S instead of through this
//! server's own push queue.

use async_trait::async_trait;
use jid::BareJid;
use minidom::Element;
use waddle_xmpp::pubsub::PubSubItem;

use super::{NotificationOutboxError, NotificationOutboxStore, PushServiceNodeName};

/// One notification for a remote Push Service.
#[derive(Debug, Clone)]
pub struct RemotePushPublication {
    pub service: BareJid,
    pub node: PushServiceNodeName,
    /// The outbox job this publishes, and the attempt, which name the IQ.
    pub job: uuid::Uuid,
    pub attempt: u32,
    pub item: PubSubItem,
    /// The publish options the user enabled the service with
    /// (XEP-0357 §4: "the publish request SHOULD include the data as
    /// publish options").
    pub publish_options: Option<Element>,
}

/// Whether the S2S route took a publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePushPublishOutcome {
    /// Queued for the service's server. The answer arrives later.
    Accepted,
    /// Not queued; a later attempt may succeed.
    Refused,
}

/// Sends publications to remote Push Services.
#[async_trait]
pub trait RemotePushPublisher: Send + Sync {
    async fn publish(&self, publication: RemotePushPublication) -> RemotePushPublishOutcome;
}

/// Where a store keeps its remote publisher, shared by its clones.
#[derive(Clone, Default)]
pub(super) struct RemotePublisherSlot(
    std::sync::Arc<std::sync::OnceLock<std::sync::Arc<dyn RemotePushPublisher>>>,
);

impl RemotePublisherSlot {
    pub(super) fn get(&self) -> Option<&std::sync::Arc<dyn RemotePushPublisher>> {
        self.0.get()
    }
}

impl NotificationOutboxStore {
    /// Let the store publish to remote Push Services. Without a publisher,
    /// only registrations of this server's own Push Service are notified.
    /// `false` when one was already attached.
    pub fn attach_remote_publisher(
        &self,
        publisher: std::sync::Arc<dyn RemotePushPublisher>,
    ) -> bool {
        self.remote_publisher.0.set(publisher).is_ok()
    }

    /// The account, Push Service and node the outbox job `job` published to.
    pub async fn job_registration(
        &self,
        job: uuid::Uuid,
    ) -> Result<Option<(BareJid, BareJid, PushServiceNodeName)>, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT recipient_bare_jid, push_service_jid, node FROM notification_outbox WHERE job_id = ?",
                crate::db_params![job.to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let recipient: String = row.get(0)?;
        let service: String = row.get(1)?;
        let node: String = row.get(2)?;
        let parse = |value: &str| {
            value
                .parse::<BareJid>()
                .map_err(|_| NotificationOutboxError::InvalidPushServiceBareJid(value.to_owned()))
        };
        Ok(Some((
            parse(&recipient)?,
            parse(&service)?,
            PushServiceNodeName::try_new(node)?,
        )))
    }
}
