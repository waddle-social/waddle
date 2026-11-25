//! Flutter bindings to the same community pubsub operations used by the browser.
pub use calendar_edit::WaddleCalendarEdit;
use minidom::Element;
use uuid::Uuid;
use waddle_xmpp_client::pubsub::build_pubsub_retract_iq;
use waddle_xmpp_client_community::calendar_edit;
use waddle_xmpp_client_community::{calendar, feed, story_reads};
use waddle_xmpp_core::xcal::{CalendarItem, PartStat, PUBSUB_NODE_EVENTS};

pub use calendar::{WaddleAttendee, WaddleCalendarItemInput, WaddleVEvent, WaddleVEventInput};
pub use feed::{WaddleFeedEntry, WaddleFeedEntryInput};
pub use story_reads::{WaddleStoryReads, WaddleStoryReadsInput};

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError};

impl WaddleClient {
    pub(crate) async fn community_iq(&self, request: Element) -> Result<Element, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let recipient = request.attr("to").map(str::to_owned).unwrap_or_else(|| {
            self.config
                .jid
                .split('/')
                .next()
                .unwrap_or_default()
                .to_owned()
        });
        let result = send_iq_with_timeout(&handle, request)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        if result.attr("from").is_some_and(|from| from != recipient) {
            return Err(WaddleError::UntrustedReply);
        }
        Ok(result)
    }

    pub async fn feed_items(
        &self,
        spaces_jid: String,
        max_items: Option<u32>,
    ) -> Result<Vec<WaddleFeedEntry>, WaddleError> {
        let service = self.require_bare_jid(&spaces_jid)?;
        let result = self
            .community_iq(feed::build_feed_items_iq(&service, max_items))
            .await?;
        Ok(feed::parse_feed_items_result(&result))
    }

    pub async fn feed_publish(
        &self,
        spaces_jid: String,
        entry: WaddleFeedEntryInput,
    ) -> Result<WaddleFeedEntry, WaddleError> {
        let service = self.require_bare_jid(&spaces_jid)?;
        let (request, published) = feed::prepare_publication(&service, entry);
        self.community_iq(request).await?;
        Ok(published)
    }

    pub async fn story_reads_fetch(&self) -> Result<WaddleStoryReads, WaddleError> {
        match self
            .community_iq(story_reads::build_story_reads_fetch_iq())
            .await
        {
            Ok(result) => Ok(WaddleStoryReads::from(
                &story_reads::parse_story_reads_fetch_result(&result),
            )),
            Err(WaddleError::Stanza { condition, .. }) if condition == "item-not-found" => {
                Ok(WaddleStoryReads::default())
            }
            Err(e) => Err(e),
        }
    }

    pub async fn story_reads_publish(
        &self,
        input: WaddleStoryReadsInput,
    ) -> Result<WaddleStoryReads, WaddleError> {
        let reads =
            story_reads::story_reads_from_input(input).map_err(|_| WaddleError::InvalidArgument)?;
        self.community_iq(story_reads::build_story_reads_publish_iq(&reads))
            .await?;
        Ok(WaddleStoryReads::from(&reads))
    }

    pub async fn xcal_items(
        &self,
        community_jid: String,
        max_items: Option<u32>,
    ) -> Result<Vec<WaddleVEvent>, WaddleError> {
        let service = self.require_bare_jid(&community_jid)?;
        let result = self
            .community_iq(calendar::build_events_items_iq(&service, max_items))
            .await?;
        Ok(calendar::parse_events_items_result(&result))
    }

    pub async fn xcal_publish(
        &self,
        community_jid: String,
        input: WaddleVEventInput,
    ) -> Result<WaddleVEvent, WaddleError> {
        let service = self.require_bare_jid(&community_jid)?;
        let id = format!("evt-{}", Uuid::new_v4());
        let event =
            calendar::vevent_from_input(&id, input).map_err(|_| WaddleError::InvalidArgument)?;
        self.community_iq(calendar::build_event_publish_iq(&service, &id, &event))
            .await?;
        Ok(WaddleVEvent::from_vevent(&id, event))
    }

    pub async fn xcal_publish_item(
        &self,
        community_jid: String,
        item_id: String,
        input: WaddleCalendarItemInput,
    ) -> Result<WaddleVEvent, WaddleError> {
        let service = self.require_bare_jid(&community_jid)?;
        let item: CalendarItem =
            calendar::item_from_input(&item_id, input).map_err(|_| WaddleError::InvalidArgument)?;
        self.community_iq(calendar::build_item_publish_iq(&service, &item_id, &item))
            .await?;
        Ok(WaddleVEvent::from_vevent(&item_id, item.master))
    }

    pub async fn edit_calendar_event(
        &self,
        community_jid: String,
        item_id: String,
        edit: WaddleCalendarEdit,
    ) -> Result<(), WaddleError> {
        if item_id.trim().is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        let service = self.require_bare_jid(&community_jid)?;
        let result = self
            .community_iq(calendar_edit::fetch_item(&service, &item_id))
            .await?;
        let update = calendar_edit::update_item(&result, &service, &item_id, edit)
            .map_err(|_| WaddleError::InvalidArgument)?;
        self.community_iq(update).await?;
        Ok(())
    }

    pub async fn xcal_retract(
        &self,
        community_jid: String,
        item_id: String,
    ) -> Result<bool, WaddleError> {
        let service = self.require_bare_jid(&community_jid)?;
        if item_id.is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        self.community_iq(build_pubsub_retract_iq(
            &Uuid::new_v4().to_string(),
            Some(&service),
            PUBSUB_NODE_EVENTS,
            &item_id,
        ))
        .await?;
        Ok(true)
    }

    pub async fn xcal_rsvp(
        &self,
        community_jid: String,
        master_uid: String,
        self_localpart: String,
        self_jid: String,
        partstat: String,
    ) -> Result<WaddleAttendee, WaddleError> {
        let service = self.require_bare_jid(&community_jid)?;
        let account = self.require_bare_jid(&self_jid)?;
        if master_uid.is_empty()
            || account
                .node()
                .is_none_or(|node| node.as_str() != self_localpart)
            || account.as_str() != self.config.jid.split('/').next().unwrap_or_default()
        {
            return Err(WaddleError::InvalidArgument);
        }
        let status = PartStat::from_str_value(&partstat).ok_or(WaddleError::InvalidArgument)?;
        self.community_iq(calendar::build_rsvp_publish_iq(
            &service,
            &master_uid,
            &self_localpart,
            account.as_str(),
            status,
        ))
        .await?;
        Ok(WaddleAttendee {
            uri: format!("xmpp:{account}"),
            partstat,
            role: None,
            rsvp: None,
        })
    }
}
