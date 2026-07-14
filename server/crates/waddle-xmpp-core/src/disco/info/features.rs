use crate::mam::{FULLTEXT_MAM_NS, WADDLE_MAM_THREAD_NS};

use super::{DISCO_INFO_NS, NS_CHATSTATES, NS_COMMANDS, NS_MUC_SELF_PING_OPTIMIZATION};

/// Feature element for disco#info response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature(pub String);

impl Feature {
    pub fn new(var: &str) -> Self {
        Self(var.to_string())
    }

    pub fn disco_info() -> Self {
        Self::new(DISCO_INFO_NS)
    }

    pub fn disco_items() -> Self {
        Self::new(super::super::items::DISCO_ITEMS_NS)
    }

    pub fn muc() -> Self {
        Self::new("http://jabber.org/protocol/muc")
    }

    pub fn mam() -> Self {
        Self::new("urn:xmpp:mam:2")
    }

    pub fn mam_extended() -> Self {
        Self::new("urn:xmpp:mam:2#extended")
    }

    pub fn fulltext_mam() -> Self {
        Self::new(FULLTEXT_MAM_NS)
    }

    pub fn waddle_mam_thread() -> Self {
        Self::new(WADDLE_MAM_THREAD_NS)
    }

    pub fn replies() -> Self {
        Self::new("urn:xmpp:reply:0")
    }

    /// Waddle's global threads view query (`urn:waddle:threads:0`).
    /// Named `threads_query` rather than `threads` to avoid colliding
    /// with the deprecated `urn:xmpp:threads:0` namespace.
    pub fn threads_query() -> Self {
        Self::new("urn:waddle:threads:0")
    }

    pub fn stanza_ids() -> Self {
        Self::new("urn:xmpp:sid:0")
    }

    pub fn message_correction() -> Self {
        Self::new("urn:xmpp:message-correct:0")
    }

    pub fn chat_markers() -> Self {
        Self::new("urn:xmpp:chat-markers:0")
    }

    pub fn chat_states() -> Self {
        Self::new(NS_CHATSTATES)
    }

    pub fn message_retraction() -> Self {
        Self::new("urn:xmpp:message-retract:1")
    }

    /// XEP-0424 §Tombstones MUST disco feature for services that
    /// replace retracted MAM entries with `<retracted/>` tombstones.
    /// Distinct from `message_retraction`: the base feature says
    /// "I understand retraction requests"; this one says "and I
    /// rewrite the archive entry rather than dropping it." Both
    /// surfaces ship on Waddle (`mam/storage/tombstone::apply_tombstone`
    /// + `protocol/event/outbound::ReplaceWithTombstone`).
    pub fn message_retraction_tombstone() -> Self {
        Self::new("urn:xmpp:message-retract:1#tombstone")
    }

    pub fn message_moderation() -> Self {
        Self::new("urn:xmpp:message-moderate:1")
    }

    pub fn reactions() -> Self {
        Self::new("urn:xmpp:reactions:0")
    }

    pub fn references() -> Self {
        Self::new("urn:xmpp:reference:0")
    }

    pub fn fallback_indication() -> Self {
        Self::new("urn:xmpp:fallback:0")
    }

    pub fn stream_management() -> Self {
        Self::new("urn:xmpp:sm:3")
    }

    pub fn roster() -> Self {
        Self::new("jabber:iq:roster")
    }

    pub fn carbons() -> Self {
        Self::new("urn:xmpp:carbons:2")
    }

    pub fn caps() -> Self {
        Self::new("http://jabber.org/protocol/caps")
    }

    pub fn roster_versioning() -> Self {
        Self::new("urn:xmpp:features:rosterver")
    }

    pub fn vcard() -> Self {
        Self::new("vcard-temp")
    }

    pub fn http_upload() -> Self {
        Self::new("urn:xmpp:http:upload:0")
    }

    pub fn push() -> Self {
        Self::new("urn:xmpp:push:0")
    }

    pub fn socks5_bytestreams() -> Self {
        Self::new("http://jabber.org/protocol/bytestreams")
    }

    pub fn last_activity() -> Self {
        Self::new("jabber:iq:last")
    }

    pub fn blocking() -> Self {
        Self::new("urn:xmpp:blocking")
    }

    pub fn ping() -> Self {
        Self::new("urn:xmpp:ping")
    }

    pub fn entity_time() -> Self {
        Self::new("urn:xmpp:time")
    }

    pub fn software_version() -> Self {
        Self::new("jabber:iq:version")
    }

    pub fn ibb() -> Self {
        Self::new("http://jabber.org/protocol/ibb")
    }

    pub fn commands() -> Self {
        Self::new(NS_COMMANDS)
    }

    pub fn csi() -> Self {
        Self::new("urn:xmpp:csi:0")
    }

    pub fn muc_self_ping_optimization() -> Self {
        Self::new(NS_MUC_SELF_PING_OPTIMIZATION)
    }

    pub fn pubsub() -> Self {
        Self::new("http://jabber.org/protocol/pubsub")
    }

    pub fn pep() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#pep")
    }

    pub fn pubsub_auto_create() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#auto-create")
    }

    pub fn pubsub_persistent_items() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#persistent-items")
    }

    pub fn pubsub_publish() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#publish")
    }

    pub fn pubsub_retrieve_items() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#retrieve-items")
    }

    pub fn bookmarks2() -> Self {
        Self::new("urn:xmpp:bookmarks:1")
    }

    /// XEP-0472 Pubsub Social Feed.
    pub fn social_feed() -> Self {
        Self::new("urn:xmpp:pubsub-social-feed:1")
    }

    /// XEP-0501 Pubsub Stories.
    pub fn stories() -> Self {
        Self::new("urn:xmpp:pubsub-social-feed:stories:0")
    }

    /// xCal calendar events per the XSF ProtoXEP
    /// "Calendaring Extensions to Publish-Subscribe". The advertised
    /// feature URI is the xCal namespace itself; the ProtoXEP has
    /// no assigned XEP number yet (status: ProtoXEP).
    pub fn xcal() -> Self {
        Self::new("urn:ietf:params:xml:ns:xcal")
    }

    pub fn bookmarks_compat() -> Self {
        Self::new("urn:xmpp:bookmarks:1#compat")
    }

    pub fn carbons_rules() -> Self {
        Self::new("urn:xmpp:carbons:rules:0")
    }

    pub fn offline_messages() -> Self {
        Self::new("msgoffline")
    }

    pub fn server_info() -> Self {
        Self::new(super::SERVER_INFO_FORM_TYPE)
    }

    pub fn occupant_id() -> Self {
        Self::new("urn:xmpp:occupant-id:0")
    }

    pub fn hats() -> Self {
        Self::new("urn:xmpp:hats:0")
    }

    pub fn explicit_mentions() -> Self {
        Self::new("urn:xmpp:mentions:0")
    }

    pub fn channel_mentions() -> Self {
        Self::new("urn:xmpp:mentions:0#channel")
    }

    pub fn muc_persistent() -> Self {
        Self::new("muc_persistent")
    }

    pub fn muc_temporary() -> Self {
        Self::new("muc_temporary")
    }

    pub fn muc_unsecured() -> Self {
        Self::new("muc_unsecured")
    }

    /// XEP-0045 §7.4 stable-id support: the service reflects groupchat
    /// messages with the original `id` intact.
    pub fn muc_stable_id() -> Self {
        Self::new("http://jabber.org/protocol/muc#stable_id")
    }

    pub fn muc_open() -> Self {
        Self::new("muc_open")
    }

    pub fn muc_membersonly() -> Self {
        Self::new("muc_membersonly")
    }

    pub fn muc_public() -> Self {
        Self::new("muc_public")
    }

    pub fn muc_hidden() -> Self {
        Self::new("muc_hidden")
    }

    pub fn muc_nonanonymous() -> Self {
        Self::new("muc_nonanonymous")
    }

    pub fn muc_unmoderated() -> Self {
        Self::new("muc_unmoderated")
    }

    pub fn muc_moderated() -> Self {
        Self::new("muc_moderated")
    }

    pub fn pubsub_subscribe() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#subscribe")
    }

    pub fn pubsub_access_whitelist() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#access-whitelist")
    }

    pub fn pubsub_publish_only_affiliation() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#publish-only-affiliation")
    }

    pub fn pubsub_access_presence() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#access-presence")
    }

    pub fn pubsub_auto_subscribe() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#auto-subscribe")
    }

    pub fn pubsub_filtered_notifications() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#filtered-notifications")
    }

    pub fn avatar_metadata_notify() -> Self {
        Self::new("urn:xmpp:avatar:metadata+notify")
    }

    /// XEP-0292 vCard4 over XMPP — the §8 MUST disco feature any
    /// client or server that supports the vCard4 namespace has to
    /// advertise. Distinct from `vcard4_notify`: this one says "I
    /// understand and serve the `urn:ietf:params:xml:ns:vcard-4.0`
    /// element"; the notify variant is the PEP `+notify` filter for
    /// XEP-0163 fan-out.
    pub fn vcard4() -> Self {
        Self::new("urn:ietf:params:xml:ns:vcard-4.0")
    }

    /// XEP-0292 vCard4 over XMPP — `+notify` filter advertised by
    /// clients that want to receive vCard4 PEP fan-out per XEP-0163 §3.
    pub fn vcard4_notify() -> Self {
        Self::new("urn:xmpp:vcard4+notify")
    }

    /// XEP-0490 Message Displayed Synchronization — payload namespace
    /// and PEP node identifier. Distinct from `mds_displayed_notify`:
    /// this one identifies the payload element; the notify variant is
    /// the PEP `+notify` filter advertised in XEP-0115 caps by the
    /// publisher's other resources so they receive the XEP-0163 §3.4
    /// owner-self fan-out.
    pub fn mds_displayed() -> Self {
        Self::new("urn:xmpp:mds:displayed:0")
    }

    /// XEP-0490 §3.2 / XEP-0163 §3 `+notify` filter advertised by chat
    /// clients participating in Message Displayed Synchronization.
    pub fn mds_displayed_notify() -> Self {
        Self::new("urn:xmpp:mds:displayed:0+notify")
    }

    pub fn private_storage() -> Self {
        Self::new("jabber:iq:private")
    }

    pub fn spaces() -> Self {
        Self::new("urn:xmpp:spaces:0")
    }

    /// XEP-0166 Jingle.
    pub fn jingle() -> Self {
        Self::new("urn:xmpp:jingle:1")
    }

    /// XEP-0167 Jingle RTP Sessions.
    pub fn jingle_rtp() -> Self {
        Self::new("urn:xmpp:jingle:apps:rtp:1")
    }

    /// XEP-0167 audio capability.
    pub fn jingle_rtp_audio() -> Self {
        Self::new("urn:xmpp:jingle:apps:rtp:audio")
    }

    /// XEP-0167 video capability.
    pub fn jingle_rtp_video() -> Self {
        Self::new("urn:xmpp:jingle:apps:rtp:video")
    }

    /// XEP-0353 Jingle Message Initiation.
    pub fn jingle_message() -> Self {
        Self::new("urn:xmpp:jingle-message:0")
    }

    /// XEP-0215 External Service Discovery.
    pub fn ext_disco() -> Self {
        Self::new("urn:xmpp:extdisco:2")
    }

    /// Custom: Waddle's LiveKit-backed Jingle transport. XEP-0166
    /// explicitly allows new Jingle transport types, so this stays
    /// conformant — Waddle keeps a custom transport namespace
    /// because no XEP defines a LiveKit-style URL+JWT credential
    /// delivery shape.
    pub fn waddle_livekit_transport() -> Self {
        Self::new("urn:waddle:transports:livekit:0")
    }

    /// XEP-0272: Multiparty Jingle (Muji). MUC group-call presence
    /// advertisement and Jingle session-initiate to the mixer JID
    /// embed `<muji xmlns='urn:xmpp:jingle:muji:0'/>`.
    pub fn muji() -> Self {
        Self::new("urn:xmpp:jingle:muji:0")
    }
}
