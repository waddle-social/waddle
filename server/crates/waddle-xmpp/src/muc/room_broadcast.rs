use std::collections::HashMap;

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid, Jid};
use tracing::{debug, instrument};
use xmpp_parsers::message::{Message, MessageType};

use super::messages::OutboundMucMessage;
use super::room::{MucRoom, Occupant};
use super::subject::{RoomSubjectTexts, SubjectState};
use crate::types::Role;
use crate::XmppError;

impl MucRoom {
    /// Broadcast a message to all occupants in the room.
    ///
    /// Per XEP-0045:
    /// - The message is sent from the room JID with sender's nick as resource
    /// - All occupants receive the message (including the sender as echo)
    /// - Visitors in moderated rooms cannot send messages
    #[instrument(
        name = "xmpp.muc.fanout",
        skip(self, message),
        fields(
            room = %self.room_jid,
            message_id = message.id.as_ref().map(|id| id.0.as_str()).unwrap_or_default(),
            recipients = tracing::field::Empty,
        )
    )]
    pub fn broadcast_message(
        &self,
        sender_nick: &str,
        message: &Message,
    ) -> Result<Vec<OutboundMucMessage>, XmppError> {
        let fanout_started = std::time::Instant::now();
        let sender = self.occupants.get(sender_nick).ok_or_else(|| {
            XmppError::forbidden(Some(format!(
                "You are not an occupant of {}",
                self.room_jid
            )))
        })?;

        if self.config.moderated && sender.role == Role::Visitor {
            return Err(XmppError::forbidden(Some(
                "Visitors cannot speak in moderated rooms".to_string(),
            )));
        }

        let from_room_jid = self
            .room_jid
            .with_resource_str(sender_nick)
            .map_err(|e| XmppError::internal(format!("Invalid nick as resource: {}", e)))?;

        debug!(
            sender = %sender_nick,
            occupant_count = self.occupants.len(),
            "Broadcasting message to room occupants"
        );

        let mut outbound = Vec::with_capacity(self.occupants.len());

        for occupant in self.occupants.values() {
            for recipient_jid in self.get_occupant_sessions(&occupant.nick) {
                let mut broadcast_msg = message.clone();
                broadcast_msg.type_ = MessageType::Groupchat;
                broadcast_msg.from = Some(Jid::from(from_room_jid.clone()));
                broadcast_msg.to = Some(Jid::from(recipient_jid.clone()));

                outbound.push(OutboundMucMessage::new(recipient_jid, broadcast_msg));
            }
        }

        tracing::Span::current().record("recipients", outbound.len());
        crate::metrics::record_muc_message();
        crate::histogram_record!(
            "waddle.muc.fanout.duration",
            "ms",
            "MUC fanout latency: groupchat broadcast accepted until every \
             per-recipient send is enqueued.",
            fanout_started.elapsed().as_secs_f64() * 1000.0,
        );

        debug!(
            message_count = outbound.len(),
            "Created broadcast messages for occupants"
        );

        Ok(outbound)
    }

    /// Find the occupant by their real JID.
    pub fn find_occupant_by_real_jid(&self, jid: &FullJid) -> Option<&Occupant> {
        self.occupants.values().find(|occupant| {
            self.get_occupant_sessions(&occupant.nick)
                .iter()
                .any(|session| session == jid)
        })
    }

    /// Find the occupant's nick by their real JID.
    pub fn find_nick_by_real_jid(&self, jid: &FullJid) -> Option<&str> {
        self.find_occupant_by_real_jid(jid).map(|o| o.nick.as_str())
    }

    /// Get all remote occupants in the room.
    pub fn get_remote_occupants(&self) -> Vec<&Occupant> {
        self.occupants.values().filter(|o| o.is_remote).collect()
    }

    /// Get all occupants grouped by their home server domain.
    pub fn get_occupants_by_domain(&self) -> HashMap<String, Vec<&Occupant>> {
        let mut by_domain: HashMap<String, Vec<&Occupant>> = HashMap::new();

        for occupant in self.occupants.values() {
            let domain = occupant
                .home_server
                .as_deref()
                .unwrap_or("local")
                .to_string();

            by_domain.entry(domain).or_default().push(occupant);
        }

        by_domain
    }

    /// Get occupants from a specific domain.
    pub fn get_occupants_for_domain(&self, domain: &str) -> Vec<&Occupant> {
        if domain == "local" {
            self.occupants
                .values()
                .filter(|o| o.home_server.is_none())
                .collect()
        } else {
            self.occupants
                .values()
                .filter(|o| o.home_server.as_deref() == Some(domain))
                .collect()
        }
    }

    /// Get the count of remote occupants.
    pub fn remote_occupant_count(&self) -> usize {
        self.occupants.values().filter(|o| o.is_remote).count()
    }

    /// Get the count of local occupants.
    pub fn local_occupant_count(&self) -> usize {
        self.occupants.values().filter(|o| !o.is_remote).count()
    }

    /// Get all unique remote server domains that have occupants in this room.
    pub fn get_remote_domains(&self) -> Vec<String> {
        let mut domains: Vec<String> = self
            .occupants
            .values()
            .filter_map(|o| o.home_server.clone())
            .collect();

        domains.sort();
        domains.dedup();
        domains
    }

    /// Apply a §8.1 subject change.
    pub fn set_subject(
        &mut self,
        texts: RoomSubjectTexts,
        setter: BareJid,
        setter_nick: String,
        set_at: DateTime<Utc>,
    ) {
        self.subject = Some(SubjectState {
            texts,
            setter,
            setter_nick,
            set_at,
        });
    }
}
