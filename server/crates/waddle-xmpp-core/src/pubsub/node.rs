//! Shared PubSub node configuration primitives.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// Bounded default for XEP-0402 bookmark PEP nodes.
///
/// XEP-0402 needs many persistent bookmark items, but an unbounded node makes
/// authenticated storage growth trivial. This is deliberately much larger than
/// normal client bookmark counts while still finite.
pub const PEP_BOOKMARK_MAX_ITEMS: u32 = 1024;

/// Access model for a PubSub node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AccessModel {
    /// Anyone can subscribe to and retrieve items.
    Open,
    /// Only entities in the owner's roster with a subscription can access.
    #[default]
    Presence,
    /// Only entities in specific roster groups can access.
    Roster,
    /// Only explicitly whitelisted JIDs can access.
    Whitelist,
    /// Only the node owner can access.
    Authorize,
}

impl fmt::Display for AccessModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AccessModel::Open => "open",
            AccessModel::Presence => "presence",
            AccessModel::Roster => "roster",
            AccessModel::Whitelist => "whitelist",
            AccessModel::Authorize => "authorize",
        };
        write!(f, "{s}")
    }
}

impl FromStr for AccessModel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(AccessModel::Open),
            "presence" => Ok(AccessModel::Presence),
            "roster" => Ok(AccessModel::Roster),
            "whitelist" => Ok(AccessModel::Whitelist),
            "authorize" => Ok(AccessModel::Authorize),
            _ => Err(()),
        }
    }
}

/// Publish model for a PubSub node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PublishModel {
    /// Only node owners can publish.
    Publishers,
    /// Only subscribers can publish.
    Subscribers,
    /// Anyone can publish.
    #[default]
    Open,
}

impl fmt::Display for PublishModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PublishModel::Publishers => "publishers",
            PublishModel::Subscribers => "subscribers",
            PublishModel::Open => "open",
        };
        write!(f, "{s}")
    }
}

impl FromStr for PublishModel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "publishers" => Ok(PublishModel::Publishers),
            "subscribers" => Ok(PublishModel::Subscribers),
            "open" => Ok(PublishModel::Open),
            _ => Err(()),
        }
    }
}

/// XEP-0060 `pubsub#type` profile identifier for a PubSub node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PubSubNodeType {
    PubsubSocialFeed,
    PubsubSocialFeedStories,
}

impl PubSubNodeType {
    pub fn as_str(self) -> &'static str {
        match self {
            PubSubNodeType::PubsubSocialFeed => crate::xep0472::NS_SOCIAL_FEED,
            PubSubNodeType::PubsubSocialFeedStories => crate::xep0501::NS_STORIES,
        }
    }
}

impl fmt::Display for PubSubNodeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for PubSubNodeType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            crate::xep0472::NS_SOCIAL_FEED => Ok(PubSubNodeType::PubsubSocialFeed),
            crate::xep0501::NS_STORIES => Ok(PubSubNodeType::PubsubSocialFeedStories),
            _ => Err(()),
        }
    }
}

/// When to send the last published item to a subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SendLastPublishedItem {
    /// Never send the last item automatically.
    Never,
    /// Send on subscription only.
    OnSub,
    /// Send on subscription and when the contact becomes available.
    #[default]
    OnSubAndPresence,
}

impl fmt::Display for SendLastPublishedItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SendLastPublishedItem::Never => "never",
            SendLastPublishedItem::OnSub => "on_sub",
            SendLastPublishedItem::OnSubAndPresence => "on_sub_and_presence",
        };
        write!(f, "{s}")
    }
}

impl FromStr for SendLastPublishedItem {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "never" => Ok(SendLastPublishedItem::Never),
            "on_sub" => Ok(SendLastPublishedItem::OnSub),
            "on_sub_and_presence" => Ok(SendLastPublishedItem::OnSubAndPresence),
            _ => Err(()),
        }
    }
}

/// Shared node configuration for PubSub / PEP nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeConfig {
    pub access_model: AccessModel,
    pub publish_model: PublishModel,
    pub node_type: Option<PubSubNodeType>,
    pub max_items: u32,
    pub persist_items: bool,
    pub deliver_payloads: bool,
    pub notify_retract: bool,
    pub notify_delete: bool,
    pub send_last_published_item: SendLastPublishedItem,
}

/// Partial node configuration submitted by an XEP-0060 configure form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeConfigPatch {
    pub access_model: Option<AccessModel>,
    pub publish_model: Option<PublishModel>,
    pub node_type: Option<PubSubNodeType>,
    pub max_items: Option<u32>,
    pub persist_items: Option<bool>,
    pub deliver_payloads: Option<bool>,
    pub notify_retract: Option<bool>,
    pub notify_delete: Option<bool>,
    pub send_last_published_item: Option<SendLastPublishedItem>,
}

impl NodeConfigPatch {
    pub fn apply_to(&self, mut config: NodeConfig) -> NodeConfig {
        if let Some(access_model) = self.access_model {
            config.access_model = access_model;
        }
        if let Some(publish_model) = self.publish_model {
            config.publish_model = publish_model;
        }
        if let Some(node_type) = self.node_type {
            config.node_type = Some(node_type);
        }
        if let Some(max_items) = self.max_items {
            config.max_items = max_items;
        }
        if let Some(persist_items) = self.persist_items {
            config.persist_items = persist_items;
        }
        if let Some(deliver_payloads) = self.deliver_payloads {
            config.deliver_payloads = deliver_payloads;
        }
        if let Some(notify_retract) = self.notify_retract {
            config.notify_retract = notify_retract;
        }
        if let Some(notify_delete) = self.notify_delete {
            config.notify_delete = notify_delete;
        }
        if let Some(send_last_published_item) = self.send_last_published_item {
            config.send_last_published_item = send_last_published_item;
        }
        config
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::pep_default()
    }
}

impl NodeConfig {
    /// XEP-0503 configuration for a public Space node.
    pub fn spaces_public() -> Self {
        Self {
            access_model: AccessModel::Open,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: u32::MAX,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// Configuration for the XEP-0472 community social-feed node.
    ///
    /// Open on both axes: anyone may subscribe/read (`access_model:
    /// Open`) AND any authenticated entity may publish (`publish_model:
    /// Open`, XEP-0060 §7.1.4). This makes the global feed
    /// member-postable, matching XEP-0472 §"Replying to a Post"
    /// ("Anyone can publish a post" to a shared feed node). The publish
    /// handler still requires an authenticated session and rejects
    /// outcasts via `can_publish`. Distinct from `spaces_public()`
    /// (Publisher publish-model) which the owner-only stories and
    /// calendar nodes keep.
    pub fn community_feed() -> Self {
        Self {
            access_model: AccessModel::Open,
            publish_model: PublishModel::Open,
            node_type: Some(PubSubNodeType::PubsubSocialFeed),
            max_items: u32::MAX,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// Configuration for the XEP-0501 community stories node.
    ///
    /// Stories are community-broadcast content, like the social feed:
    /// any authenticated, non-outcast member may publish. The server
    /// still stamps the authenticated publisher into the story payload
    /// before storing it.
    pub fn community_stories() -> Self {
        Self {
            access_model: AccessModel::Open,
            publish_model: PublishModel::Open,
            node_type: Some(PubSubNodeType::PubsubSocialFeedStories),
            max_items: u32::MAX,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// XEP-0503 configuration for a private Space node.
    pub fn spaces_private() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: u32::MAX,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// Default configuration for PEP nodes.
    pub fn pep_default() -> Self {
        Self {
            access_model: AccessModel::Presence,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: 1,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSubAndPresence,
        }
    }

    /// Default configuration for a concrete PEP node.
    pub fn pep_for_node(node: &str) -> Self {
        let mut config = Self::pep_default();
        if node == super::pep::PEP_NODE_BOOKMARKS {
            config.max_items = PEP_BOOKMARK_MAX_ITEMS;
            config = config.normalize_xep0402_bookmarks();
        } else if node == super::pep::PEP_NODE_MDS_DISPLAYED {
            config = Self::mds_displayed();
        } else if node == super::pep::PEP_NODE_STICKERS {
            config = Self::stickers_defaults();
        } else if node == super::pep::PEP_NODE_VCARD4 {
            config = Self::vcard4_defaults();
        } else if node == super::pep::PEP_NODE_WADDLE_DND {
            config = Self::waddle_dnd_defaults();
        } else if node == super::pep::PEP_NODE_WADDLE_DM_BOOKMARKS {
            config = Self::waddle_dm_bookmarks_defaults();
        } else if node == crate::waddle_status_preference::PEP_NODE_WADDLE_STATUS_PREFERENCE {
            config = Self::waddle_status_preference_defaults();
        } else if node == crate::waddle_story_reads::PEP_NODE_WADDLE_STORY_READS {
            config = Self::waddle_story_reads_defaults();
        }
        config
    }

    /// Whether a node's access model is pinned to `whitelist` by its
    /// well-known PEP defaults (issue #1094).
    ///
    /// Fan-out privacy is derived from the persisted `access_model`,
    /// so nodes whose well-known default is `whitelist` must never be
    /// stored with anything weaker — neither via an owner
    /// configure-set nor as a legacy row created before the default
    /// existed. Consumers use this single predicate for both the
    /// configure-time clamp and the read-time backfill so the pin set
    /// always tracks the `pep_for_node` table and never becomes a
    /// second hand-maintained node-name list.
    pub fn pins_whitelist_access(node: &str) -> bool {
        Self::pep_for_node(node).access_model == AccessModel::Whitelist
    }

    /// Defaults for the Waddle story-reads PEP node.
    ///
    /// The single `current` item is the user's private story read-state
    /// (which stories they have seen, and when) — no roster contact may
    /// read it. The wasm client requests exactly this shape via
    /// publish-options, but publish-options are not enforced at the
    /// server's Publish arm, so the auto-create default MUST already be
    /// `whitelist` + `never`: with the generic `pep_default()`
    /// (`access_model = presence`) the access_model-derived fan-out
    /// gate (issue #1094) would let the XEP-0163 §3 roster pass deliver
    /// read-state to any contact advertising
    /// `urn:waddle:story:reads:0+notify`.
    ///
    /// `notify_retract` / `notify_delete` are `false` because the
    /// server does not emit those events for this node; read-state
    /// changes are always republishes of the `current` item.
    pub fn waddle_story_reads_defaults() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: 1,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: false,
            notify_delete: false,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }

    /// Defaults for the Waddle status-preference PEP node (ADR-010
    /// Phase 4).
    ///
    /// The synced manual-status payload is owner-only private state: it
    /// follows the user across their own resources but no roster contact
    /// needs to read it (contacts see only the resulting `<show>` on the
    /// normal presence broadcast). Force `whitelist` access and `never`
    /// send-last-published so a fresh roster subscription never receives
    /// it; the user's own resources read it via `<items/>` GET on connect
    /// and adopt live changes via the XEP-0163 §3.4 self fan-out.
    /// `max_items = 1` keeps the node single-slot (`id = current`).
    ///
    /// `notify_retract` / `notify_delete` are `false` because the server
    /// does not emit the XEP-0060 §7.2.2 / §9.1.5 retract/delete events
    /// on the wire — advertising them as `true` would lie to subscribers,
    /// and reset is published as an explicit `mode='automatic'` item
    /// rather than a retract precisely so it fans out as a publish.
    pub fn waddle_status_preference_defaults() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: 1,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: false,
            notify_delete: false,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }

    /// Defaults for the Waddle DM-bookmarks PEP node (issue #720).
    ///
    /// The DM notification-settings carrier hosts one official
    /// XEP-0492 `<notify>` per direct-chat contact, keyed by the
    /// contact's bare JID. Like the XEP-0402 MUC bookmarks node, it
    /// holds many persistent items, so it shares that node's bounded
    /// cap (`PEP_BOOKMARK_MAX_ITEMS`). The cap is deliberate anti-DoS
    /// parity with the bookmarks node: an unbounded `max_items` would
    /// disable eviction entirely (see `enforce_max_items_tx`), letting
    /// an authenticated client grow its own DM node without limit. The
    /// cap is far larger than any realistic per-contact override count
    /// while still finite. The authoritative consumer is the
    /// server-side projection consulted at the T1 push gate — no roster
    /// contact needs to read this node. Force `whitelist` access and
    /// `never` send-last-published so a fresh roster subscription does
    /// NOT receive the user's per-contact notification overrides.
    ///
    /// `notify_retract` / `notify_delete` are `false` because the
    /// server does not currently emit the XEP-0060 §7.2.2 / §9.1.5
    /// retract/delete events on the wire — advertising them as `true`
    /// would lie to subscribers. The user's other resources resync via
    /// `<items/>` GET on resume, not via retract fanout.
    pub fn waddle_dm_bookmarks_defaults() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: PEP_BOOKMARK_MAX_ITEMS,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: false,
            notify_delete: false,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }

    /// Defaults for the Waddle DND PEP node (issue #367).
    ///
    /// The payload contains personally-identifying schedule data
    /// (timezone, sleep hours, snooze deadlines). The authoritative
    /// consumer is the server-side projection consulted at the T1
    /// push gate — no roster contact needs to read this node. Force
    /// `whitelist` access and `never` send-last-published so a fresh
    /// roster subscription does NOT receive the user's DND state.
    /// `max_items = 1` keeps the node single-slot (the PEP idiom for
    /// single-item nodes is `id = current`).
    ///
    /// `notify_retract` / `notify_delete` are set to `false` because
    /// the server does not currently emit the XEP-0060 §7.2.2 /
    /// §9.1.5 retract/delete events on the wire — advertising them
    /// as `true` would lie to subscribers. Bob's other resources
    /// resync DND state via `<items/>` GET on resume, not via
    /// retract fanout. See PR #759 / round-6 XMPP-conformance review.
    pub fn waddle_dnd_defaults() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: 1,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: false,
            notify_delete: false,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }

    /// XEP-0449 sticker-pack PEP node defaults.
    ///
    /// One pubsub item per sticker pack (item id = the pack id derived
    /// from the pack hash), so the node needs many persistent items —
    /// but a FINITE cap, not `u32::MAX`: an unbounded `max_items`
    /// disables eviction entirely (see `enforce_max_items_tx`), letting
    /// an authenticated client grow its own sticker node without limit.
    /// It shares the XEP-0402 bookmarks node's `PEP_BOOKMARK_MAX_ITEMS`
    /// anti-DoS cap, which is far larger than any realistic pack
    /// collection while still finite.
    ///
    /// `access_model = open` per XEP-0449 ("The pubsub node's access
    /// model SHOULD be set to 'open', so that other users can fetch
    /// sticker packs"). `send_last_published_item = never`: peers fetch
    /// packs on demand via `<items/>` GET; a multi-kilobyte pack payload
    /// must not ride the roster presence fan-out. `notify_retract` /
    /// `notify_delete` are `false` because the server does not emit the
    /// XEP-0060 §7.2.2 / §9.1.5 retract/delete events on the wire —
    /// advertising them as `true` would lie to subscribers; the user's
    /// other resources resync packs via `<items/>` GET.
    pub fn stickers_defaults() -> Self {
        Self {
            access_model: AccessModel::Open,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: PEP_BOOKMARK_MAX_ITEMS,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: false,
            notify_delete: false,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }

    /// XEP-0292 §6.1 vCard4 PEP node defaults.
    ///
    /// XEP-0292 §6.1 pins `open` as the canonical access model for the
    /// `urn:xmpp:vcard4` node so any peer (roster relationship or not)
    /// can resolve a user's published vCard4. `max_items = 1` keeps the
    /// node single-slot so each publish replaces the prior one rather
    /// than growing an unbounded item history — matching the
    /// OIDC-managed publisher in `waddle-server::profile::publish`.
    pub fn vcard4_defaults() -> Self {
        Self {
            access_model: AccessModel::Open,
            max_items: 1,
            ..Self::pep_default()
        }
    }

    /// XEP-0490 §3 Message Displayed Synchronization node defaults.
    ///
    /// The XEP mandates the publishing client send these as
    /// publish-options preconditions:
    ///   - `pubsub#persist_items=true`
    ///   - `pubsub#max_items=max`
    ///   - `pubsub#access_model=whitelist`
    ///   - `pubsub#send_last_published_item=never`
    ///
    /// We bake these into the well-known node defaults so auto-create
    /// matches the spec without depending on the (currently unenforced)
    /// publish-options precondition path. `max=max` maps to `u32::MAX`.
    pub fn mds_displayed() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: u32::MAX,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }

    /// Force XEP-0402 bookmark privacy and durability invariants onto a config.
    pub fn normalize_xep0402_bookmarks(mut self) -> Self {
        self.access_model = AccessModel::Whitelist;
        self.publish_model = PublishModel::Publishers;
        if self.max_items == 0 || self.max_items > PEP_BOOKMARK_MAX_ITEMS {
            self.max_items = PEP_BOOKMARK_MAX_ITEMS;
        }
        self.persist_items = true;
        self.send_last_published_item = SendLastPublishedItem::Never;
        self
    }

    /// Force the `urn:waddle:dm-bookmarks:0` privacy and durability
    /// invariants onto a config.
    ///
    /// Applied on every owner reconfigure (not just auto-create) so the
    /// anti-DoS `max_items` cap and the whitelist/send-last privacy of the
    /// DM carrier cannot be reverted via `pubsub#owner` configure — full
    /// parity with [`Self::normalize_xep0402_bookmarks`] on the sibling
    /// MUC node. Without this, an owner could set `max_items=max`,
    /// re-disabling eviction, or `access_model=open`, leaking which
    /// contacts they have muted.
    pub fn normalize_waddle_dm_bookmarks(mut self) -> Self {
        self.access_model = AccessModel::Whitelist;
        self.publish_model = PublishModel::Publishers;
        if self.max_items == 0 || self.max_items > PEP_BOOKMARK_MAX_ITEMS {
            self.max_items = PEP_BOOKMARK_MAX_ITEMS;
        }
        self.persist_items = true;
        self.send_last_published_item = SendLastPublishedItem::Never;
        self
    }

    /// Configuration for a public node.
    pub fn public() -> Self {
        Self {
            access_model: AccessModel::Open,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: 10,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// Configuration for a whitelist-only node.
    pub fn whitelist() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: 10,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// XEP-0357 Push Service node defaults.
    ///
    /// A Push Service node is a delivery target, not a subscriber-visible feed:
    /// use the XEP-0357 recommended whitelist access model and publisher-gated
    /// writes, keep only a bounded durable item trail, and never replay the last
    /// push notification to subscribers.
    pub fn push_service() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            node_type: None,
            max_items: 10_000,
            persist_items: true,
            deliver_payloads: false,
            notify_retract: false,
            notify_delete: false,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_model_round_trips() {
        assert_eq!("open".parse::<AccessModel>(), Ok(AccessModel::Open));
        assert_eq!("presence".parse::<AccessModel>(), Ok(AccessModel::Presence));
        assert_eq!(AccessModel::Whitelist.to_string(), "whitelist");
    }

    #[test]
    fn publish_model_round_trips() {
        assert_eq!(
            "publishers".parse::<PublishModel>(),
            Ok(PublishModel::Publishers)
        );
        assert_eq!(PublishModel::Open.to_string(), "open");
    }

    #[test]
    fn pubsub_node_type_round_trips() {
        assert_eq!(
            crate::xep0472::NS_SOCIAL_FEED.parse::<PubSubNodeType>(),
            Ok(PubSubNodeType::PubsubSocialFeed)
        );
        assert_eq!(
            PubSubNodeType::PubsubSocialFeedStories.to_string(),
            crate::xep0501::NS_STORIES
        );
    }

    #[test]
    fn community_feed_is_open_read_and_open_publish() {
        let config = NodeConfig::community_feed();
        assert_eq!(config.access_model, AccessModel::Open);
        assert_eq!(config.publish_model, PublishModel::Open);
        assert_eq!(config.node_type, Some(PubSubNodeType::PubsubSocialFeed));
        assert!(config.persist_items);
        assert!(config.deliver_payloads);
        // Differs from spaces_public() only on publish-model: the feed
        // is member-postable, spaces nodes are Publisher-gated.
        assert_eq!(
            NodeConfig::spaces_public().publish_model,
            PublishModel::Publishers
        );
    }

    #[test]
    fn community_stories_sets_xep0501_node_type() {
        let config = NodeConfig::community_stories();
        assert_eq!(
            config.node_type,
            Some(PubSubNodeType::PubsubSocialFeedStories)
        );
    }

    #[test]
    fn node_config_defaults_match_pep() {
        let config = NodeConfig::default();
        assert_eq!(config, NodeConfig::pep_default());
        assert_eq!(config.max_items, 1);
        assert!(config.persist_items);
    }

    #[test]
    fn bookmarks_pep_config_keeps_multiple_private_items() {
        let config = NodeConfig::pep_for_node(super::super::pep::PEP_NODE_BOOKMARKS);
        assert_eq!(config.access_model, AccessModel::Whitelist);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, PEP_BOOKMARK_MAX_ITEMS);
        assert!(config.persist_items);
        assert_eq!(
            config.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }

    #[test]
    fn bookmarks_normalization_preserves_bounded_retention() {
        let mut config = NodeConfig::spaces_public();
        config.max_items = 10;

        let normalized = config.normalize_xep0402_bookmarks();

        assert_eq!(normalized.access_model, AccessModel::Whitelist);
        assert_eq!(normalized.publish_model, PublishModel::Publishers);
        assert_eq!(normalized.max_items, 10);
        assert!(normalized.persist_items);
        assert_eq!(
            normalized.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }

    #[test]
    fn spaces_configs_do_not_inherit_pep_single_item_retention() {
        let public = NodeConfig::spaces_public();
        assert_eq!(public.access_model, AccessModel::Open);
        assert_eq!(public.max_items, u32::MAX);
        assert!(public.persist_items);
        assert!(public.notify_retract);

        let private = NodeConfig::spaces_private();
        assert_eq!(private.access_model, AccessModel::Whitelist);
        assert_eq!(private.max_items, u32::MAX);
        assert!(private.persist_items);
        assert!(private.notify_retract);
    }

    #[test]
    fn stickers_pep_config_is_open_with_bounded_retention() {
        // XEP-0449: the auto-create path on a publish to
        // `urn:xmpp:stickers:0` MUST land Open — with the generic
        // `pep_default()` (`access_model = presence`) non-roster peers
        // could never fetch a referenced pack even though the XEP says
        // the node SHOULD be open. One item per pack means many
        // persistent items, but with the finite bookmarks anti-DoS cap
        // so eviction still runs.
        let config = NodeConfig::pep_for_node(super::super::pep::PEP_NODE_STICKERS);
        assert_eq!(config.access_model, AccessModel::Open);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, PEP_BOOKMARK_MAX_ITEMS);
        assert_ne!(
            config.max_items,
            u32::MAX,
            "sticker node must NOT be unbounded — eviction would never run"
        );
        assert!(config.persist_items);
        assert!(config.deliver_payloads);
        assert!(!config.notify_retract);
        assert!(!config.notify_delete);
        assert_eq!(
            config.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }

    #[test]
    fn stickers_defaults_helper_matches_pep_for_node() {
        // `stickers_defaults()` is the single source of truth the
        // publish-handler reconcile path compares existing-node configs
        // against, so `pep_for_node` MUST return the same value.
        assert_eq!(
            NodeConfig::pep_for_node(super::super::pep::PEP_NODE_STICKERS),
            NodeConfig::stickers_defaults()
        );
    }

    #[test]
    fn vcard4_pep_config_is_open_with_single_item_retention() {
        // XEP-0292 §6.1 canonical access model: `open`. The auto-create
        // path on a publish to `urn:xmpp:vcard4` MUST land Open, not
        // Presence — otherwise non-roster peers can't read the vCard
        // even though the XEP says the node is publicly readable.
        let config = NodeConfig::pep_for_node(super::super::pep::PEP_NODE_VCARD4);
        assert_eq!(config.access_model, AccessModel::Open);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, 1);
        assert!(config.persist_items);
    }

    #[test]
    fn vcard4_defaults_helper_matches_pep_for_node() {
        // Defence-in-depth: `vcard4_defaults()` is the single source of
        // truth that the publish-handler reconcile path will compare
        // existing-node configs against, so `pep_for_node` MUST return
        // the same value.
        assert_eq!(
            NodeConfig::pep_for_node(super::super::pep::PEP_NODE_VCARD4),
            NodeConfig::vcard4_defaults()
        );
    }

    #[test]
    fn waddle_dm_bookmarks_pep_config_is_whitelist_with_bounded_retention() {
        // The DM notification-settings carrier (#720) holds one item per
        // direct-chat contact, so it needs many persistent items — but a
        // FINITE cap, not `u32::MAX`. A `u32::MAX` cap would disable
        // `enforce_max_items_tx` entirely, letting an authenticated client
        // grow its own DM node unbounded. It shares the XEP-0402 bookmarks
        // node's `PEP_BOOKMARK_MAX_ITEMS` anti-DoS cap. Like DND it must
        // stay off the roster fan-out (whitelist + send-last=never).
        let config = NodeConfig::pep_for_node(super::super::pep::PEP_NODE_WADDLE_DM_BOOKMARKS);
        assert_eq!(config.access_model, AccessModel::Whitelist);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, PEP_BOOKMARK_MAX_ITEMS);
        assert_ne!(
            config.max_items,
            u32::MAX,
            "DM node must NOT be unbounded — eviction would never run"
        );
        assert!(config.persist_items);
        assert!(!config.notify_retract);
        assert!(!config.notify_delete);
        assert_eq!(
            config.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }

    #[test]
    fn waddle_dm_bookmarks_defaults_helper_matches_pep_for_node() {
        // `waddle_dm_bookmarks_defaults()` is the single source of truth
        // the publish-handler reconcile path compares existing-node
        // configs against, so `pep_for_node` MUST return the same value.
        assert_eq!(
            NodeConfig::pep_for_node(super::super::pep::PEP_NODE_WADDLE_DM_BOOKMARKS),
            NodeConfig::waddle_dm_bookmarks_defaults()
        );
    }

    #[test]
    fn waddle_status_preference_pep_config_is_single_slot_whitelist() {
        // The synced manual-status payload (ADR-010 Phase 4) is owner-only
        // private state — only the user's own resources read it, never
        // roster contacts — so it must stay off the roster fan-out
        // (whitelist + send-last=never), single-slot (`id = current`).
        let node = crate::waddle_status_preference::PEP_NODE_WADDLE_STATUS_PREFERENCE;
        let config = NodeConfig::pep_for_node(node);
        assert_eq!(config.access_model, AccessModel::Whitelist);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, 1);
        assert!(config.persist_items);
        assert!(config.deliver_payloads);
        assert!(!config.notify_retract);
        assert!(!config.notify_delete);
        assert_eq!(
            config.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }

    #[test]
    fn waddle_status_preference_defaults_helper_matches_pep_for_node() {
        let node = crate::waddle_status_preference::PEP_NODE_WADDLE_STATUS_PREFERENCE;
        assert_eq!(
            NodeConfig::pep_for_node(node),
            NodeConfig::waddle_status_preference_defaults()
        );
    }

    #[test]
    fn push_service_config_matches_xep0357_delivery_target_defaults() {
        let config = NodeConfig::push_service();

        assert_eq!(config.access_model, AccessModel::Whitelist);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert!(config.persist_items);
        assert!(!config.deliver_payloads);
        assert_eq!(
            config.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }
}
