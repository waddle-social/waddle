//! Live room and PEP lookups for rich push notifications.

use std::sync::Arc;

use async_trait::async_trait;
use jid::{BareJid, DomainPart, Jid};
use kameo::actor::ActorRef;
use minidom::Element;
use waddle_xmpp::muc::room_actor::{GetConfig, GetOccupantByNick};
use waddle_xmpp::muc::room_registry_actor::{GetRoom, RoomRegistryActor};
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::push::names::{DisplayName, PushNames};
use waddle_xmpp::xep::xep0292::{is_vcard4_element, parse_vcard4, PEP_NODE_VCARD4};

use crate::auth::local_account_jid_exists;
use crate::db::DatabasePool;
use crate::notification_outbox::{NotificationClass, PushDisplayNames};
use crate::pubsub_authz::{pep_read_decision, ContactDirectory, ReadDecision};

pub(crate) struct PublishedPushDisplayNames {
    pub(crate) registry: ActorRef<RoomRegistryActor>,
    pub(crate) pubsub: Arc<dyn PubSubStorage>,
    pub(crate) db_pool: Arc<DatabasePool>,
    pub(crate) local_domain: DomainPart,
}

impl PublishedPushDisplayNames {
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
        DisplayName::parse(parse_vcard4(&vcard).display_name()?)
    }

    async fn room_names(&self, recipient: &BareJid, room: &BareJid, sender: &Jid) -> PushNames {
        let Ok(Some(room_actor)) = self
            .registry
            .ask(GetRoom {
                room_jid: room.clone(),
            })
            .await
        else {
            return PushNames::default();
        };
        let Ok(config) = room_actor.ask(GetConfig).await else {
            return PushNames::default();
        };
        let room = DisplayName::parse(&config.name);
        // Waddle currently supports only non-anonymous rooms. If that
        // changes, require the recipient to be allowed to see real JIDs
        // before resolving an occupant's account name.
        let sender = match sender.resource() {
            Some(nick) => match room_actor
                .ask(GetOccupantByNick {
                    nick: nick.to_string(),
                })
                .await
            {
                Ok(Some(occupant)) => {
                    self.account_name(&occupant.real_jid.to_bare(), recipient)
                        .await
                }
                _ => None,
            },
            None => None,
        };
        PushNames { sender, room }
    }
}

#[async_trait]
impl PushDisplayNames for PublishedPushDisplayNames {
    async fn resolve(
        &self,
        recipient: &BareJid,
        conversation: &BareJid,
        sender: &Jid,
        class: NotificationClass,
    ) -> PushNames {
        match class {
            NotificationClass::Call => PushNames::default(),
            NotificationClass::DirectMessage
            | NotificationClass::DirectMessageMention
            | NotificationClass::MissedCall => PushNames {
                sender: self.account_name(&sender.to_bare(), recipient).await,
                room: None,
            },
            NotificationClass::PersonalMention
            | NotificationClass::ChannelMention
            | NotificationClass::ActiveChannelMention
            | NotificationClass::NotifyAll => {
                self.room_names(recipient, conversation, sender).await
            }
        }
    }
}
