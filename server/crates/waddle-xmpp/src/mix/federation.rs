//! Server-to-server delivery primitives for MIX.
//!
//! MIX channels can have participants on remote servers. When a message is
//! published to a channel, it must be fanned out to each participant's
//! server via S2S. This module carries the typed effects used by the
//! dispatcher to emit those messages; actual wire transmission lives in
//! the S2S layer.

use jid::BareJid;
use xmpp_parsers::message::Message;

use super::channel::MixChannel;

/// A single federated delivery of a MIX message to a remote participant's
/// server. The sender (channel JID) and recipient (participant bare JID)
/// are separated so the S2S layer can route by recipient domain.
#[derive(Debug, Clone)]
pub struct FederatedMixDelivery {
    pub from: BareJid,
    pub to: BareJid,
    pub message: Message,
}

/// Compute the set of federated deliveries required for a MIX message.
///
/// Local participants receive their copy via the same-server fan-out path;
/// this function returns only the remote deliveries. The sender itself is
/// always excluded from the delivery set.
pub fn plan_federated_deliveries(
    channel: &MixChannel,
    local_domain: &str,
    sender: &BareJid,
    message: Message,
) -> Vec<FederatedMixDelivery> {
    let mut out = Vec::new();
    for participant in channel.participants() {
        if participant.real_jid.domain().as_str() == local_domain {
            continue;
        }
        if &participant.real_jid == sender {
            continue;
        }
        out.push(FederatedMixDelivery {
            from: channel.channel_jid.clone(),
            to: participant.real_jid.clone(),
            message: message.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mix::channel::{MixChannelConfig, Participant};
    use xmpp_parsers::message::MessageType;

    fn channel_jid() -> BareJid {
        "general@mix.example.com".parse().unwrap()
    }

    #[test]
    fn test_plan_skips_local_participants_and_sender() {
        let mut ch = MixChannel::new(
            channel_jid(),
            "w".into(),
            "c".into(),
            MixChannelConfig::default(),
        );
        let sender: BareJid = "alice@example.com".parse().unwrap();
        ch.upsert_participant(Participant::new(sender.clone(), "Alice"));
        ch.upsert_participant(Participant::new("bob@example.com".parse().unwrap(), "Bob"));
        ch.upsert_participant(Participant::new(
            "carol@remote.example.net".parse().unwrap(),
            "Carol",
        ));
        ch.upsert_participant(Participant::new(
            "dave@other.example.org".parse().unwrap(),
            "Dave",
        ));

        let msg = Message::new(Some(jid::Jid::from(channel_jid())));
        let mut msg = msg;
        msg.type_ = MessageType::Groupchat;
        let plan = plan_federated_deliveries(&ch, "example.com", &sender, msg);
        assert_eq!(plan.len(), 2);
        let targets: Vec<_> = plan.iter().map(|d| d.to.to_string()).collect();
        assert!(targets.contains(&"carol@remote.example.net".into()));
        assert!(targets.contains(&"dave@other.example.org".into()));
    }

    #[test]
    fn test_plan_empty_when_all_local() {
        let mut ch = MixChannel::new(
            channel_jid(),
            "w".into(),
            "c".into(),
            MixChannelConfig::default(),
        );
        ch.upsert_participant(Participant::new(
            "alice@example.com".parse().unwrap(),
            "Alice",
        ));
        ch.upsert_participant(Participant::new("bob@example.com".parse().unwrap(), "Bob"));
        let sender: BareJid = "alice@example.com".parse().unwrap();
        let msg = Message::new(Some(jid::Jid::from(channel_jid())));
        let plan = plan_federated_deliveries(&ch, "example.com", &sender, msg);
        assert!(plan.is_empty());
    }
}
