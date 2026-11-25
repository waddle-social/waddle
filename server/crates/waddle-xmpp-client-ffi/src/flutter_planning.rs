use crate::{WaddleClient, WaddleError, WaddlePlanningItem};
use waddle_xmpp_client_community::planning;

impl WaddleClient {
    pub async fn event_room_create(
        &self,
        name: String,
        private_room: bool,
    ) -> Result<String, WaddleError> {
        use crate::error::client_error_to_waddle;
        use waddle_xmpp_client::discovery::DiscoveryExt;
        use waddle_xmpp_client::messaging::MessagingExt;
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            return Err(WaddleError::InvalidArgument);
        }
        let owner = self
            .config
            .jid
            .parse::<jid::Jid>()
            .map_err(|_| WaddleError::InvalidJid)?
            .to_bare();
        let nick = owner.node().ok_or(WaddleError::InvalidJid)?.to_string();
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let services = handle
            .discover_component_services(owner.domain().as_str())
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        let localpart = format!("event-{}", uuid::Uuid::new_v4());
        let node = jid::NodePart::new(&localpart).map_err(|_| WaddleError::InvalidJid)?;
        let room = jid::BareJid::from_parts(Some(&node), services.muc.domain());
        handle
            .join_room(room.as_str(), &nick)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        let result = waddle_xmpp_client_community::plan_workspace::configure(
            &room,
            name,
            nick,
            private_room,
            |request| self.community_iq(request),
            |_| WaddleError::MalformedResponse,
        )
        .await;
        if let Err(error) = result {
            let _ = self
                .destroy_room(
                    room.to_string(),
                    Some("Event setup did not complete".into()),
                )
                .await;
            return Err(error);
        }
        Ok(room.to_string())
    }

    pub async fn event_planning_items(
        &self,
        room_jid: String,
    ) -> Result<Vec<WaddlePlanningItem>, WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let result = self.community_iq(planning::items(&room, None)).await?;
        planning::parse_items(&result, &room).map_err(|_| WaddleError::MalformedResponse)
    }

    pub async fn event_planning_publish(
        &self,
        room_jid: String,
        input: Box<WaddlePlanningItem>,
    ) -> Result<WaddlePlanningItem, WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let owner = self
            .config
            .jid
            .parse::<jid::Jid>()
            .map_err(|_| WaddleError::InvalidJid)?
            .to_bare();
        let (id, iq) =
            planning::publish(&room, owner, *input).map_err(|_| WaddleError::InvalidArgument)?;
        self.community_iq(iq).await?;
        let result = self.community_iq(planning::items(&room, Some(&id))).await?;
        planning::published(&result, &room, &id).map_err(|_| WaddleError::MalformedResponse)
    }

    pub async fn event_planning_retract(
        &self,
        room_jid: String,
        item_id: String,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        self.community_iq(
            planning::retract(&room, &item_id).map_err(|_| WaddleError::InvalidArgument)?,
        )
        .await?;
        Ok(())
    }
}
