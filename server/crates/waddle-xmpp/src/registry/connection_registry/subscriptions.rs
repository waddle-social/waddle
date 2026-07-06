use super::*;

impl ConnectionRegistry {
    /// Queue a subscription stanza for an offline bare JID.
    ///
    /// These stanzas are delivered when the user next becomes available.
    pub fn queue_pending_subscription_stanza(&self, bare_jid: &BareJid, stanza: Stanza) {
        let mut pending = self
            .pending_subscription_stanzas
            .entry(bare_jid.clone())
            .or_default();
        if let Stanza::Presence(presence) = &stanza {
            if presence.type_ == xmpp_parsers::presence::Type::Subscribe {
                if let Some(requester) = presence.from.as_ref().map(|from| from.to_bare()) {
                    pending.retain(|queued| {
                        !matches!(
                            queued,
                            Stanza::Presence(queued_presence)
                                if queued_presence.type_ == xmpp_parsers::presence::Type::Subscribe
                                    && queued_presence
                                        .from
                                        .as_ref()
                                        .is_some_and(|from| from.to_bare() == requester)
                        )
                    });
                }
            }
        }
        pending.push(stanza);
    }

    /// Remove queued inbound subscribe stanzas from `requester` to `recipient`.
    pub fn remove_pending_subscribe(&self, recipient: &BareJid, requester: &BareJid) -> usize {
        let Some(mut entry) = self.pending_subscription_stanzas.get_mut(recipient) else {
            return 0;
        };
        let before = entry.len();
        entry.retain(|stanza| {
            !matches!(
                stanza,
                Stanza::Presence(presence)
                    if presence.type_ == xmpp_parsers::presence::Type::Subscribe
                        && presence
                            .from
                            .as_ref()
                            .is_some_and(|from| from.to_bare() == *requester)
            )
        });
        before - entry.len()
    }

    /// Drain and return all pending subscription stanzas for a bare JID.
    pub fn drain_pending_subscription_stanzas(&self, bare_jid: &BareJid) -> Vec<Stanza> {
        self.pending_subscription_stanzas
            .remove(bare_jid)
            .map(|(_, stanzas)| stanzas)
            .unwrap_or_default()
    }

    /// Return queued subscription stanzas for a bare JID without removing
    /// them. RFC 6121 §3.1.3: pending inbound subscribe requests stay queued
    /// until approved or denied and are delivered once per session, on each
    /// fresh resource's initial available presence (the caller gates on
    /// `ConnectionEntry::claim_pending_subscribes_flush`, issue #1104) — not
    /// on every subsequent presence update within a session.
    pub fn pending_subscription_stanzas(&self, bare_jid: &BareJid) -> Vec<Stanza> {
        self.pending_subscription_stanzas
            .get(bare_jid)
            .map(|stanzas| stanzas.clone())
            .unwrap_or_default()
    }
}
