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
            "MUC fanout latency: groupchat broadcast accepted until the \
             per-recipient outbound set is built.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Affiliation, Role};
    use std::io;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn room_with_two_occupants() -> MucRoom {
        let mut room = MucRoom::new(
            "team@muc.example.com".parse().expect("room jid"),
            "waddle-1".into(),
            "channel-1".into(),
            Default::default(),
        );
        for (real_jid, nick) in [
            ("alice@example.com/browser", "alice"),
            ("bob@example.com/phone", "bob"),
        ] {
            room.add_occupant(Occupant {
                real_jid: real_jid.parse().expect("occupant jid"),
                nick: nick.into(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
                is_remote: false,
                home_server: None,
            });
        }
        room
    }

    fn correlated_message() -> Message {
        let mut message = Message::new(None::<Jid>);
        message.id = Some(xmpp_parsers::message::Id("fanout-correlation-1".into()));
        message.type_ = MessageType::Groupchat;
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "sensitive fanout body".into(),
        );
        message
    }

    #[test]
    fn fanout_span_carries_room_message_id_and_recipient_count_without_body() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(CaptureWriter(Arc::clone(&bytes)))
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            room_with_two_occupants()
                .broadcast_message("alice", &correlated_message())
                .expect("broadcast succeeds");
        });

        let output = String::from_utf8(bytes.lock().expect("capture lock").clone())
            .expect("captured tracing is UTF-8");
        for expected in [
            "\"room\":\"team@muc.example.com\"",
            "\"message_id\":\"fanout-correlation-1\"",
            "\"recipients\":2",
        ] {
            assert!(output.contains(expected), "missing {expected} in {output}");
        }
        assert!(
            !output.contains("sensitive fanout body"),
            "message bodies must never be tracing fields: {output}"
        );
    }
}
