//! Shared XEP-0424 / XEP-0425 tombstone matching.
//!
//! One matcher, two consumers: the XEP-0198 SM session registry scrub
//! (`stream_management::session_registry`) and the XEP-0160
//! `pending_delivery` scrub (in-memory impl here, libSQL/Postgres impl
//! in `waddle-server`). Keeping the per-stanza predicate in one place
//! guarantees a retraction removes the SAME set of cached copies from
//! every replay-capable store.

/// Typed identity of a XEP-0424 / XEP-0425 tombstone target.
///
/// XEP-0424 §"Using the correct ID" distinguishes how a retraction
/// names its target: groupchat retractions use the room-assigned
/// XEP-0359 `<stanza-id/>` (`by` == room bare JID); all other types
/// use the sender's client-chosen wire `id`. The wire id is NOT
/// unique across senders, so an untyped `(target_id, archive_jid)`
/// pair let one sender's retraction scrub a colliding message from a
/// DIFFERENT author queued for the same recipient. Carrying the id
/// kind plus the author/room identity makes the match precise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TombstoneTarget {
    /// Groupchat retraction / XEP-0425 moderation: keyed by the
    /// room-assigned XEP-0359 stanza-id. A cached reflection matches
    /// ONLY on `<stanza-id xmlns='urn:xmpp:sid:0' id=stanza_id
    /// by=room>` — never on the client-chosen wire `id` attribute.
    Groupchat {
        /// The room-assigned archive id (== wire stanza-id per the
        /// "archive id == wire stanza-id" invariant).
        stanza_id: String,
        /// The room's bare JID — both the conversation scope and the
        /// required `by` of the matched stanza-id.
        room: jid::BareJid,
    },
    /// 1:1 retraction: keyed by the retracting author's wire id. A
    /// cached message matches on (wire `id` == `wire_id` AND
    /// from-bare == `author`), or on an archive-stamped
    /// `<stanza-id id=wire_id by=archive>`; both branches are scoped
    /// to the conversation archive.
    Direct {
        /// The retracted message's wire `id` (client-chosen, unique
        /// only per author).
        wire_id: String,
        /// Bare JID of the retraction author — the original message's
        /// sender. A colliding wire id from any other sender must
        /// never match.
        author: jid::BareJid,
        /// The archive (conversation) scope: the message's `from` or
        /// `to` must bare-equal this JID.
        archive: jid::BareJid,
    },
    /// Groupchat retraction cascade: a cached XEP-0308 correction of
    /// the retracted message. XEP-0424 §Tombstones applies a retraction
    /// to the original and each of its corrections, and XEP-0308 §Business
    /// Rules allows a MUC correction only from the original's full JID, so
    /// a cached reflection matches iff it carries
    /// `<replace xmlns='urn:xmpp:message-correct:0' id=replaced_id/>` and
    /// its `from` equals the original author's occupant JID. A cached
    /// reflection is addressed `from` the occupant, never by a
    /// room stanza-id, so this variant never consults `<stanza-id/>`.
    GroupchatCorrection {
        /// The id a correction names in `<replace id=…/>`: one of the
        /// original's identities (room stanza-id, wire id, origin-id).
        replaced_id: String,
        /// The original author's occupant JID (`room@service/nick`).
        occupant: jid::FullJid,
        /// The room's bare JID — the conversation scope.
        room: jid::BareJid,
    },
}

impl TombstoneTarget {
    /// The target message id string (room stanza-id, author wire id, or
    /// the id a correction replaces).
    pub fn id(&self) -> &str {
        match self {
            Self::Groupchat { stanza_id, .. } => stanza_id,
            Self::Direct { wire_id, .. } => wire_id,
            Self::GroupchatCorrection { replaced_id, .. } => replaced_id,
        }
    }

    /// The conversation (archive) scope of the tombstone: the room's
    /// bare JID for groupchat, the archive owner's bare JID for 1:1.
    pub fn archive_jid(&self) -> &jid::BareJid {
        match self {
            Self::Groupchat { room, .. } => room,
            Self::Direct { archive, .. } => archive,
            Self::GroupchatCorrection { room, .. } => room,
        }
    }

    /// Per-stanza tombstone predicate. A cached message matches iff:
    ///   1. it is a `<message>` element,
    ///   2. its `from` or `to` attribute bare-equals
    ///      [`Self::archive_jid`] (scope guard — prevents
    ///      cross-conversation collateral damage when short message
    ///      ids collide across chats), AND
    ///   3. the variant-specific identity matches:
    ///      - [`Self::Groupchat`]: a `<stanza-id
    ///        xmlns='urn:xmpp:sid:0' id=stanza_id by=room/>` child
    ///        (XEP-0424 groupchat retractions key by the room's
    ///        XEP-0359 stamp; the client-chosen wire `id` is never
    ///        consulted),
    ///      - [`Self::Direct`]: wire `id` equals `wire_id` AND the
    ///        message's `from` bare-equals `author`, OR an
    ///        archive-stamped `<stanza-id id=wire_id by=archive/>`
    ///        child, OR a XEP-0308 correction of the target: a
    ///        `<replace xmlns='urn:xmpp:message-correct:0' id=wire_id/>`
    ///        child on a message whose `from` bare-equals `author`
    ///        (XEP-0424 §Tombstones: a retraction applies to the
    ///        original and each of its corrections). A retraction
    ///        stanza that carries a `<replace/>` fallback is never
    ///        treated as a correction.
    ///
    /// XEP-0359 §3 scopes `<stanza-id/>` to `urn:xmpp:sid:0`; the
    /// namespace is matched explicitly so an unrelated extension
    /// element named "stanza-id" cannot trigger a scrub, and the `by`
    /// attribute is verified so an occupant-forged stanza-id cannot
    /// either.
    pub fn matches_message_element(&self, el: &minidom::Element) -> bool {
        if el.name() != "message" {
            return false;
        }
        let archive_jid = self.archive_jid();
        let in_scope = el
            .attr("from")
            .map(|s| jid_bare_equals(s, archive_jid))
            .unwrap_or(false)
            || el
                .attr("to")
                .map(|s| jid_bare_equals(s, archive_jid))
                .unwrap_or(false);
        if !in_scope {
            return false;
        }
        match self {
            Self::Groupchat { stanza_id, room } => has_stanza_id_by(el, stanza_id, room),
            Self::Direct {
                wire_id,
                author,
                archive,
            } => {
                let wire_match = el.attr("id") == Some(wire_id.as_str())
                    && el
                        .attr("from")
                        .map(|s| jid_bare_equals(s, author))
                        .unwrap_or(false);
                wire_match
                    || has_stanza_id_by(el, wire_id, archive)
                    || is_correction_by(el, wire_id, author)
            }
            Self::GroupchatCorrection {
                replaced_id,
                occupant,
                ..
            } => is_correction_from(el, replaced_id, |from| {
                from.parse::<jid::Jid>()
                    .map(|from| from.try_into_full().ok().as_ref() == Some(occupant))
                    .unwrap_or(false)
            }),
        }
    }
}

/// True iff `el` carries a XEP-0359 `<stanza-id xmlns='urn:xmpp:sid:0'
/// id=target_id/>` child whose `by` bare-equals `by_jid`.
fn has_stanza_id_by(el: &minidom::Element, target_id: &str, by_jid: &jid::BareJid) -> bool {
    el.children().any(|c| {
        c.is("stanza-id", "urn:xmpp:sid:0")
            && c.attr("id") == Some(target_id)
            && c.attr("by")
                .map(|by| jid_bare_equals(by, by_jid))
                .unwrap_or(false)
    })
}

/// True iff `el` is a XEP-0308 correction of `target_id` sent by `author`:
/// it carries `<replace xmlns='urn:xmpp:message-correct:0' id=target_id/>`,
/// its `from` bare-equals `author`, and it is not itself a XEP-0424
/// retraction (whose fallback may carry a `<replace/>` that MUST NOT be
/// applied as a correction and must still be delivered).
fn is_correction_by(el: &minidom::Element, target_id: &str, author: &jid::BareJid) -> bool {
    is_correction_from(el, target_id, |from| jid_bare_equals(from, author))
}

/// [`is_correction_by`] with a caller-supplied author predicate over the
/// `from` attribute (bare-JID for 1:1, full occupant JID for MUC).
fn is_correction_from(
    el: &minidom::Element,
    target_id: &str,
    from_author: impl Fn(&str) -> bool,
) -> bool {
    el.attr("from").is_some_and(from_author)
        && !el
            .children()
            .any(|c| c.is("retract", crate::xep::xep0424::NS_MESSAGE_RETRACT))
        && el.children().any(|c| {
            c.is("replace", crate::xep::xep0308::NS_MESSAGE_CORRECT)
                && c.attr("id") == Some(target_id)
        })
}

/// Select the sequences of unacked outbound `<message/>` entries that
/// match a XEP-0424 / XEP-0425 tombstone. Pure — takes a snapshot of
/// `(sequence, stanza_xml)` pairs so the caller can parse OUTSIDE the
/// registry's write locks (issue #1145) and later remove exactly the
/// returned `(stream_id, sequence)` pairs, which is safe even if the
/// queue changed between snapshot and removal.
///
/// Parse errors and non-message frames are skipped silently — only
/// matching messages are selected.
pub fn matching_tombstone_sequences(
    entries: &[(u32, String)],
    target: &TombstoneTarget,
) -> Vec<u32> {
    entries
        .iter()
        .filter(
            |(_, stanza_xml)| match stanza_xml.parse::<minidom::Element>() {
                Ok(el) => target.matches_message_element(&el),
                Err(_) => false,
            },
        )
        .map(|(sequence, _)| *sequence)
        .collect()
}

fn jid_bare_equals(jid_str: &str, archive_jid: &jid::BareJid) -> bool {
    match jid_str.parse::<jid::Jid>() {
        Ok(jid) => &jid.to_bare() == archive_jid,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(s: &str) -> jid::BareJid {
        s.parse().expect("valid bare jid")
    }

    fn direct(wire_id: &str, author: &str, archive: &str) -> TombstoneTarget {
        TombstoneTarget::Direct {
            wire_id: wire_id.to_string(),
            author: bare(author),
            archive: bare(archive),
        }
    }

    fn message_from_to(from: &str, to: &str, wire_id: &str) -> minidom::Element {
        format!("<message xmlns='jabber:client' from='{from}' to='{to}' id='{wire_id}'/>")
            .parse()
            .expect("valid message element")
    }

    #[test]
    fn matches_are_scoped_by_normalized_bare_jid_not_raw_string() {
        // The archive scope and author identity are typed BareJids, so
        // IDNA/nodeprep normalization applies on both sides: a stanza
        // addressed with mixed-case JIDs matches a target whose JIDs
        // normalize to the same values. A raw-string comparison would
        // have missed this and silently skipped the scrub, leaking the
        // retracted message.
        let target = direct("retract-me", "alice@example.com", "bob@example.com");
        let el = message_from_to("Alice@EXAMPLE.com/web", "Bob@Example.COM", "retract-me");
        assert!(target.matches_message_element(&el));
    }

    #[test]
    fn groupchat_stanza_id_by_is_bare_jid_normalized() {
        let target = TombstoneTarget::Groupchat {
            stanza_id: "room-id-1".to_string(),
            room: bare("room@conference.example.com"),
        };
        let el: minidom::Element =
            "<message xmlns='jabber:client' from='Room@Conference.EXAMPLE.com/alice' \
             to='bob@example.com/web' id='wire' type='groupchat'>\
             <stanza-id xmlns='urn:xmpp:sid:0' by='Room@Conference.EXAMPLE.com' id='room-id-1'/>\
             </message>"
                .parse()
                .expect("valid message element");
        assert!(target.matches_message_element(&el));
    }

    #[test]
    fn does_not_match_a_different_conversation() {
        let target = direct("retract-me", "alice@example.com", "bob@example.com");
        let el = message_from_to("alice@example.com/web", "carol@example.com", "retract-me");
        assert!(!target.matches_message_element(&el));
    }

    #[test]
    fn direct_retraction_does_not_scrub_another_senders_colliding_wire_id() {
        // FINDING A repro: Mallory sends Bob a message whose wire id
        // collides with Alice's undelivered message to Bob, then
        // retracts her own message. The wire id is client-chosen and
        // non-unique, so a 1:1 retraction must also verify the cached
        // message's author (from-bare) — Alice's message must survive.
        let mallorys_retraction = direct("collide", "mallory@example.com", "bob@example.com");
        let alices_message = message_from_to("alice@example.com/web", "bob@example.com", "collide");
        assert!(
            !mallorys_retraction.matches_message_element(&alices_message),
            "a retraction must not scrub a colliding wire id from a different author"
        );
        // Mallory's own message DOES match.
        let mallorys_message =
            message_from_to("mallory@example.com/web", "bob@example.com", "collide");
        assert!(mallorys_retraction.matches_message_element(&mallorys_message));
    }

    #[test]
    fn groupchat_retraction_does_not_match_client_chosen_wire_id() {
        // FINDING A repro: XEP-0424 groupchat retractions target the
        // room-assigned XEP-0359 stanza-id (by == room bare JID), never
        // the client-chosen wire @id. Matching the wire id lets any
        // occupant mint a colliding id and scrub someone else's
        // reflection.
        let target = TombstoneTarget::Groupchat {
            stanza_id: "victim-target".to_string(),
            room: bare("room@conference.example.com"),
        };
        let reflection: minidom::Element =
            "<message xmlns='jabber:client' from='room@conference.example.com/alice' \
             to='bob@example.com/web' id='victim-target' type='groupchat'>\
             <body>hi</body>\
             <stanza-id xmlns='urn:xmpp:sid:0' by='room@conference.example.com' id='room-id-1'/>\
             </message>"
                .parse()
                .expect("valid message element");
        assert!(
            !target.matches_message_element(&reflection),
            "groupchat scrub must key on the room stanza-id, not the wire id"
        );
    }

    #[test]
    fn stanza_id_match_requires_by_to_equal_archive() {
        // FINDING A repro: the stanza-id branch previously ignored the
        // `by` attribute entirely, so any occupant-injected fake
        // <stanza-id id=target by=attacker/> triggered the scrub.
        let target = TombstoneTarget::Groupchat {
            stanza_id: "room-id-1".to_string(),
            room: bare("room@conference.example.com"),
        };
        let forged: minidom::Element =
            "<message xmlns='jabber:client' from='room@conference.example.com/mallory' \
             to='bob@example.com/web' type='groupchat'>\
             <stanza-id xmlns='urn:xmpp:sid:0' by='mallory@example.com' id='room-id-1'/>\
             </message>"
                .parse()
                .expect("valid message element");
        assert!(
            !target.matches_message_element(&forged),
            "a stanza-id whose by= is not the archive must never match"
        );
    }

    fn correction(from: &str, to: &str, replaces: &str) -> minidom::Element {
        format!(
            "<message xmlns='jabber:client' from='{from}' to='{to}' id='fix-1'>\
             <body>fixed</body>\
             <replace xmlns='urn:xmpp:message-correct:0' id='{replaces}'/></message>"
        )
        .parse()
        .expect("valid message element")
    }

    #[test]
    fn direct_retraction_scrubs_same_author_corrections_of_the_target() {
        let target = direct("orig-1", "alice@example.com", "bob@example.com");
        let el = correction("Alice@Example.com/web", "bob@example.com", "orig-1");
        assert!(target.matches_message_element(&el));
        let other_target = correction("alice@example.com/web", "bob@example.com", "orig-2");
        assert!(!target.matches_message_element(&other_target));
    }

    #[test]
    fn direct_retraction_never_scrubs_another_authors_replace() {
        let target = direct("orig-1", "alice@example.com", "bob@example.com");
        let el = correction("bob@example.com/web", "alice@example.com", "orig-1");
        assert!(!target.matches_message_element(&el));
        let mallory = correction("mallory@example.com/web", "bob@example.com", "orig-1");
        assert!(!target.matches_message_element(&mallory));
    }

    #[test]
    fn replace_in_a_retraction_fallback_is_not_scrubbed_as_a_correction() {
        let target = direct("orig-1", "alice@example.com", "bob@example.com");
        let retraction: minidom::Element =
            "<message xmlns='jabber:client' from='alice@example.com/web' to='bob@example.com' \
             id='r1'><retract xmlns='urn:xmpp:message-retract:1' id='orig-1'/>\
             <replace xmlns='urn:xmpp:message-correct:0' id='orig-1'/></message>"
                .parse()
                .expect("valid message element");
        assert!(!target.matches_message_element(&retraction));
    }

    #[test]
    fn replace_in_another_namespace_is_not_a_correction() {
        let target = direct("orig-1", "alice@example.com", "bob@example.com");
        let el: minidom::Element =
            "<message xmlns='jabber:client' from='alice@example.com/web' to='bob@example.com' \
             id='x'><replace xmlns='urn:example:other' id='orig-1'/></message>"
                .parse()
                .expect("valid message element");
        assert!(!target.matches_message_element(&el));
    }

    fn occupant_correction(from: &str, replaces: &str) -> minidom::Element {
        format!(
            "<message xmlns='jabber:client' from='{from}' to='bob@example.com/web' \
             id='fix-1' type='groupchat'><body>fixed</body>\
             <replace xmlns='urn:xmpp:message-correct:0' id='{replaces}'/></message>"
        )
        .parse()
        .expect("valid message element")
    }

    fn groupchat_correction_target() -> TombstoneTarget {
        TombstoneTarget::GroupchatCorrection {
            replaced_id: "orig-1".to_string(),
            occupant: "room@conference.example.com/alice"
                .parse()
                .expect("occupant jid"),
            room: bare("room@conference.example.com"),
        }
    }

    #[test]
    fn groupchat_retraction_scrubs_same_occupant_corrections() {
        let target = groupchat_correction_target();
        let el = occupant_correction("Room@Conference.EXAMPLE.com/alice", "orig-1");
        assert!(target.matches_message_element(&el));
        let other_id = occupant_correction("room@conference.example.com/alice", "orig-2");
        assert!(!target.matches_message_element(&other_id));
    }

    #[test]
    fn groupchat_retraction_never_scrubs_another_occupants_correction() {
        let target = groupchat_correction_target();
        // Same room and replaced id, different nick: a different occupant
        // (XEP-0308 requires the full JID to match).
        let mallory = occupant_correction("room@conference.example.com/mallory", "orig-1");
        assert!(!target.matches_message_element(&mallory));
        // The room's own bare JID is not the occupant.
        let room = occupant_correction("room@conference.example.com", "orig-1");
        assert!(!target.matches_message_element(&room));
        // Same nick in another room.
        let elsewhere = occupant_correction("other@conference.example.com/alice", "orig-1");
        assert!(!target.matches_message_element(&elsewhere));
    }

    #[test]
    fn groupchat_correction_target_ignores_stanza_id_and_retraction_fallbacks() {
        let target = groupchat_correction_target();
        let stamped: minidom::Element =
            "<message xmlns='jabber:client' from='room@conference.example.com/alice' \
             to='bob@example.com/web' type='groupchat'>\
             <stanza-id xmlns='urn:xmpp:sid:0' by='room@conference.example.com' id='orig-1'/>\
             </message>"
                .parse()
                .expect("valid message element");
        assert!(!target.matches_message_element(&stamped));
        let retraction: minidom::Element =
            "<message xmlns='jabber:client' from='room@conference.example.com/alice' \
             to='bob@example.com/web' type='groupchat'>\
             <retract xmlns='urn:xmpp:message-retract:1' id='orig-1'/>\
             <replace xmlns='urn:xmpp:message-correct:0' id='orig-1'/></message>"
                .parse()
                .expect("valid message element");
        assert!(!target.matches_message_element(&retraction));
    }
}
