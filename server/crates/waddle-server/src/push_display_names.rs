//! Published room and PEP lookups for rich push notifications.

use std::sync::Arc;

use async_trait::async_trait;
use jid::{BareJid, DomainPart, Jid};
use kameo::actor::ActorRef;
use minidom::Element;
use waddle_xmpp::mam::MamStorage;
use waddle_xmpp::muc::room_actor::GetConfig;
use waddle_xmpp::muc::room_registry_actor::{GetRoom, RoomRegistryActor};
use waddle_xmpp::muc::{MucDurableStore, RoomConfig};
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::push::names::{DisplayName, PushNames};
use waddle_xmpp::xep::xep0292::{is_vcard4_element, parse_vcard4, PEP_NODE_VCARD4};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::MessageType;

use crate::auth::local_account_jid_exists;
use crate::db::DatabasePool;
use crate::notification_outbox::{PushDisplayNameRequest, PushDisplayNameResolver};
use crate::pubsub_authz::{pep_read_decision, ContactDirectory, ReadDecision};
use crate::server::routes::websocket::WebSocketState;

pub(crate) struct PublishedPushDisplayNameResolver {
    registry: ActorRef<RoomRegistryActor>,
    durable_rooms: Option<Arc<dyn MucDurableStore>>,
    mam: Arc<dyn MamStorage>,
    pubsub: Arc<dyn PubSubStorage>,
    db_pool: Arc<DatabasePool>,
    local_domain: DomainPart,
}

impl PublishedPushDisplayNameResolver {
    pub(crate) fn new(state: &WebSocketState) -> Self {
        let deps = &state.deps;
        Self {
            registry: deps.protocol.room_registry.clone(),
            #[cfg(feature = "clustering")]
            durable_rooms: deps.app_state.clustering_claims.muc_durable_store.clone(),
            #[cfg(not(feature = "clustering"))]
            durable_rooms: None,
            mam: deps.protocol.mam_storage.clone(),
            pubsub: deps.protocol.pubsub_storage.clone(),
            db_pool: deps.app_state.db_pool.clone(),
            local_domain: deps
                .auth_state
                .caps_server_domain
                .as_jid()
                .domain()
                .to_owned(),
        }
    }

    async fn account_name(&self, sender: &BareJid, recipient: &BareJid) -> Option<DisplayName> {
        if !local_account_jid_exists(
            self.db_pool.global_actor(),
            sender,
            self.local_domain.as_str(),
        )
        .await
        .ok()?
        {
            return None;
        }
        let contacts = ContactDirectory::new(&self.db_pool);
        if pep_read_decision(&self.pubsub, &contacts, sender, PEP_NODE_VCARD4, recipient)
            .await
            .ok()?
            != ReadDecision::Allowed
        {
            return None;
        }
        let item = self
            .pubsub
            .get_items(sender, PEP_NODE_VCARD4, Some(1), &[])
            .await
            .ok()?
            .into_iter()
            .next()?;
        if item.publisher.as_ref() != Some(sender) {
            return None;
        }
        let vcard = item.payload_xml?.parse::<Element>().ok()?;
        if !is_vcard4_element(&vcard) {
            return None;
        }
        let vcard = parse_vcard4(&vcard);
        vcard
            .full_name
            .as_deref()
            .and_then(DisplayName::parse)
            .or_else(|| vcard.nickname.as_deref().and_then(DisplayName::parse))
    }

    async fn room_config(&self, room: &BareJid) -> Option<RoomConfig> {
        if let Some(store) = &self.durable_rooms {
            if let Ok(Some(config)) = store.load_published_room_config(room).await {
                return Some(config);
            }
        }
        // Standalone and non-persistent rooms only have a live config.
        let actor = self
            .registry
            .ask(GetRoom {
                room_jid: room.clone(),
            })
            .await
            .ok()??;
        actor.ask(GetConfig).await.ok()
    }

    async fn archived_room_sender(
        &self,
        room: &BareJid,
        occupant: &Jid,
        stanza_id: &StanzaId,
    ) -> Option<BareJid> {
        if stanza_id.by != *room || occupant.resource().is_none() {
            return None;
        }
        // The room-issued stanza ID is the archive primary key. The archive
        // snapshot records the author at message time, even after nick reuse.
        let message = self.mam.get_message(stanza_id.as_str()).await.ok()??;
        if message.to != stanza_id.by
            || message.from != *occupant
            || message.message_type != MessageType::Groupchat
        {
            return None;
        }
        // Waddle rooms are non-anonymous. If that changes, require permission
        // to see real JIDs before using the room-authored sender snapshot.
        Some(message.rich?.muc_sender?.jid.to_bare())
    }
}

#[async_trait]
impl PushDisplayNameResolver for PublishedPushDisplayNameResolver {
    async fn resolve(&self, request: PushDisplayNameRequest<'_>) -> PushNames {
        let PushDisplayNameRequest {
            recipient,
            conversation,
            sender,
            class,
            archive_stanza_id,
        } = request;
        if !class.is_room() {
            return PushNames {
                sender: self.account_name(&sender.to_bare(), recipient).await,
                room: None,
            };
        }
        let room = self
            .room_config(conversation)
            .await
            .and_then(|config| DisplayName::parse(&config.name));
        let sender = match archive_stanza_id {
            Some(stanza_id) => match self
                .archived_room_sender(conversation, sender, &stanza_id)
                .await
            {
                Some(account) => self.account_name(&account, recipient).await,
                None => None,
            },
            None => None,
        };
        PushNames { sender, room }
    }
}
