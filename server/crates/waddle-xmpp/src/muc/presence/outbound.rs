use jid::FullJid;
use xmpp_parsers::presence::Presence;

/// An outbound MUC presence to send to an occupant.
#[derive(Debug, Clone)]
pub struct OutboundMucPresence {
    /// The recipient's full JID
    pub to: FullJid,
    /// The presence to send
    pub presence: Presence,
}

impl OutboundMucPresence {
    /// Create a new outbound presence.
    pub fn new(to: FullJid, presence: Presence) -> Self {
        Self { to, presence }
    }
}
