use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::stanza_error::StanzaError;

use crate::request::StanzaId;

use super::namespaces::{NS_CLIENT, NS_MUC_USER};

/// An RFC 6120 message error, before any echoed payload can produce effects.
/// Applications must correlate both the ID and sender with their outbound send.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRejection {
    pub stanza_id: StanzaId,
    pub from: Jid,
    pub to: Option<Jid>,
    pub error: StanzaError,
}

impl MessageRejection {
    pub(crate) fn parse(element: &Element, default_from: Option<&BareJid>) -> Option<Self> {
        if !element.is("message", NS_CLIENT) || element.attr("type") != Some("error") {
            return None;
        }
        let mut errors = element
            .children()
            .filter(|child| child.is("error", NS_CLIENT));
        let error = StanzaError::try_from(errors.next()?.clone()).ok()?;
        if errors.next().is_some() {
            return None;
        }
        Some(Self {
            stanza_id: StanzaId::new(element.attr("id")?).ok()?,
            from: match element.attr("from") {
                Some(from) => from.parse().ok()?,
                // RFC 6120 §8.1.2.1: only a direct stream stanza can inherit
                // the user's account address; forwarded messages cannot.
                None => default_from?.clone().into(),
            },
            to: element.attr("to").map(str::parse).transpose().ok()?,
            error,
        })
    }

    /// Correlate only the retained SM tail. Acknowledged sends are owned by the
    /// application; the runtime does not keep a second outbound history.
    pub(crate) fn matches_outbound(&self, outbound: &Element, account: &BareJid) -> bool {
        if !outbound.is("message", NS_CLIENT)
            || outbound.attr("id") != Some(self.stanza_id.as_str())
            || self.to.as_ref().is_some_and(|to| to.to_bare() != *account)
        {
            return false;
        }
        let Some(recipient) = outbound.attr("to").and_then(|to| to.parse::<Jid>().ok()) else {
            return false;
        };
        let from_domain = self.from.node().is_none() && self.from.resource().is_none();
        if from_domain
            && (self.from.domain() == account.domain() || self.from.domain() == recipient.domain())
        {
            return true;
        }
        let muc = outbound.attr("type") == Some("groupchat")
            || outbound.get_child("x", NS_MUC_USER).is_some();
        if muc {
            // XEP-0045 §7.5: occupant errors retain the full occupant address;
            // room/service errors can originate at the room or service itself.
            self.from == recipient
                || (self.from.resource().is_none() && self.from.to_bare() == recipient.to_bare())
        } else {
            self.from.to_bare() == recipient.to_bare()
        }
    }
}
