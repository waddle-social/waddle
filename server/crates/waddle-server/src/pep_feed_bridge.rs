//! Community activity → social feed bridge.
//!
//! Observes successful PEP publishes (mood / activity / tune /
//! avatar / vCard4) AND successful RSVP publishes on the calendar
//! events node, and shadow-publishes a typed feed entry to
//! `urn:xmpp:pubsub-social-feed:0` on the community service so the
//! Feed pane surfaces user activity automatically alongside manual
//! posts.
//!
//! ## Design
//!
//! - Bridge entries are published as the community service itself
//!   (no `publisher` JID on the wire) so the originating user
//!   doesn't need Publisher affiliation on the community node.
//!   The entry's `<author>` field carries their bare JID for chat-
//!   side attribution.
//! - A `<source xmlns='urn:waddle:feed-source:0' kind='...'/>`
//!   typed child distinguishes bridged entries from manual posts so
//!   the chat can render a kind-icon (mood / activity / tune /
//!   avatar / vcard).
//! - Per-(user, kind) throttle suppresses high-cadence updates
//!   (tune in particular). Re-publishes that produce the same
//!   summary string are also suppressed.
//! - `WADDLE_PEP_FEED_BRIDGE_ENABLED=0` disables the bridge without
//!   a redeploy.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use minidom::Element;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{debug, warn};
use uuid::Uuid;
use waddle_xmpp::pubsub::{PubSubItem, PubSubStorage};

const NS_MOOD: &str = "http://jabber.org/protocol/mood";
const NS_ACTIVITY: &str = "http://jabber.org/protocol/activity";
const NS_TUNE: &str = "http://jabber.org/protocol/tune";
const NS_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";
const NS_VCARD4: &str = "urn:ietf:params:xml:ns:vcard-4.0";

const NS_FEED_SOURCE: &str = "urn:waddle:feed-source:0";

const COOLDOWN_DEFAULT: Duration = Duration::from_secs(5 * 60);
const COOLDOWN_TUNE: Duration = Duration::from_secs(30 * 60);

const FEATURE_ENV: &str = "WADDLE_PEP_FEED_BRIDGE_ENABLED";

/// Which kind of activity a bridged feed entry summarises. Matches
/// the `<source kind=.../>` attribute the chat reads to render the
/// right kind-icon next to the entry. Despite the type name (kept
/// stable to avoid a cross-repo rename), this also covers non-PEP
/// surfaces such as calendar RSVPs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PepKind {
    Mood,
    Activity,
    Tune,
    Avatar,
    VCard,
    Rsvp,
}

impl PepKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Mood => "mood",
            Self::Activity => "activity",
            Self::Tune => "tune",
            Self::Avatar => "avatar",
            Self::VCard => "vcard",
            Self::Rsvp => "rsvp",
        }
    }

    fn cooldown(self) -> Duration {
        match self {
            Self::Tune => COOLDOWN_TUNE,
            _ => COOLDOWN_DEFAULT,
        }
    }

    /// Map a PEP node namespace to a `PepKind`, or `None` when the
    /// node is not a PEP we bridge. RSVPs come in via a separate
    /// publish path (calendar events node) and aren't covered here.
    pub fn from_node(node: &str) -> Option<Self> {
        match node {
            NS_MOOD => Some(Self::Mood),
            NS_ACTIVITY => Some(Self::Activity),
            NS_TUNE => Some(Self::Tune),
            NS_AVATAR_METADATA => Some(Self::Avatar),
            NS_VCARD4 => Some(Self::VCard),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct ThrottleSlot {
    last_at: Instant,
    last_summary: String,
}

/// Bridge state shared across all PEP publishes. Holds throttle
/// state per-(user, kind) so a rapid-fire publisher (e.g. Tune
/// updating every track) doesn't spam the community feed.
pub struct PepFeedBridge {
    enabled: bool,
    throttle: Mutex<HashMap<(BareJid, PepKind), ThrottleSlot>>,
    /// Separate throttle for RSVPs so each (user, event-uid) gets its
    /// own slot — sharing a single PepKind::Rsvp slot would suppress
    /// legitimate RSVPs to *different* events within the cooldown.
    rsvp_throttle: Mutex<HashMap<(BareJid, String), ThrottleSlot>>,
}

impl PepFeedBridge {
    /// Construct a new bridge. Reads `WADDLE_PEP_FEED_BRIDGE_ENABLED`
    /// for the on/off flag (defaults to enabled).
    pub fn new() -> Self {
        let enabled = std::env::var(FEATURE_ENV)
            .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(true);
        Self {
            enabled,
            throttle: Mutex::new(HashMap::new()),
            rsvp_throttle: Mutex::new(HashMap::new()),
        }
    }

    /// Observe a PEP publish. Returns immediately if the bridge is
    /// disabled, the node isn't a bridged PEP, or the throttle
    /// suppresses this update. Otherwise builds and publishes the
    /// feed entry, returning the published item id on success.
    ///
    /// Failures are logged at WARN and swallowed — a PEP publish
    /// must not fail because the bridge couldn't post.
    pub async fn observe<S: PubSubStorage + ?Sized>(
        &self,
        storage: &Arc<S>,
        community_jid: &BareJid,
        author_jid: &BareJid,
        node: &str,
        published: &PubSubItem,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let kind = PepKind::from_node(node)?;
        let summary = render_summary(kind, published)?;
        if !self.admit(author_jid.clone(), kind, &summary).await {
            debug!(
                author = %author_jid,
                kind = ?kind,
                "PEP→feed bridge: suppressed by throttle"
            );
            return None;
        }

        let item_id = format!("pep-{}-{}", kind.as_str(), Uuid::new_v4());
        let entry = build_bridge_entry(&item_id, kind, author_jid, &summary);
        let item = PubSubItem {
            id: Some(item_id.clone()),
            publisher: None,
            payload: Some(entry),
        };

        match storage
            .publish_item(
                community_jid,
                waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED,
                &item,
                None,
                false,
            )
            .await
        {
            Ok(result) => {
                debug!(
                    author = %author_jid,
                    kind = ?kind,
                    item_id = %result.item_id,
                    "PEP→feed bridge: published"
                );
                Some(result.item_id)
            }
            Err(error) => {
                warn!(
                    author = %author_jid,
                    kind = ?kind,
                    error = %error,
                    "PEP→feed bridge: publish failed"
                );
                None
            }
        }
    }

    /// Observe a successful RSVP publish. Looks up the master event
    /// to grab its SUMMARY, renders a feed entry like "is going to
    /// Friday Game Night", and shadow-publishes it. Suppressed when
    /// the bridge is disabled, the master can't be found, or the
    /// per-(author, master-uid) throttle fires (so toggling Going →
    /// Maybe → Going within the cooldown only produces one entry
    /// per change).
    pub async fn observe_rsvp<S: PubSubStorage + ?Sized>(
        &self,
        storage: &Arc<S>,
        community_jid: &BareJid,
        author_jid: &BareJid,
        master_uid: &str,
        partstat: waddle_xmpp_core::xcal::PartStat,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let master = lookup_master_event(storage, community_jid, master_uid).await?;
        let event_label = if master.summary.is_empty() {
            "an event".to_string()
        } else {
            master.summary.clone()
        };
        let summary = render_rsvp_summary(partstat, &event_label);
        if !self
            .admit_rsvp(author_jid.clone(), master_uid.to_string(), &summary)
            .await
        {
            debug!(
                author = %author_jid,
                master_uid,
                "RSVP→feed bridge: suppressed by throttle"
            );
            return None;
        }
        let item_id = format!("rsvp-{}-{}", short_uid(master_uid), Uuid::new_v4());
        let entry = build_bridge_entry(&item_id, PepKind::Rsvp, author_jid, &summary);
        let item = PubSubItem {
            id: Some(item_id.clone()),
            publisher: None,
            payload: Some(entry),
        };
        match storage
            .publish_item(
                community_jid,
                waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED,
                &item,
                None,
                false,
            )
            .await
        {
            Ok(result) => {
                debug!(
                    author = %author_jid,
                    master_uid,
                    item_id = %result.item_id,
                    "RSVP→feed bridge: published"
                );
                Some(result.item_id)
            }
            Err(error) => {
                warn!(
                    author = %author_jid,
                    master_uid,
                    error = %error,
                    "RSVP→feed bridge: publish failed"
                );
                None
            }
        }
    }

    async fn admit_rsvp(&self, author: BareJid, master_uid: String, summary: &str) -> bool {
        let mut guard = self.rsvp_throttle.lock().await;
        let now = Instant::now();
        let cooldown = COOLDOWN_DEFAULT;
        match guard.get(&(author.clone(), master_uid.clone())) {
            Some(slot) if slot.last_summary == summary => false,
            Some(slot) if now.duration_since(slot.last_at) < cooldown => false,
            _ => {
                guard.insert(
                    (author, master_uid),
                    ThrottleSlot {
                        last_at: now,
                        last_summary: summary.to_string(),
                    },
                );
                true
            }
        }
    }

    /// Return `true` when this (author, kind, summary) should be
    /// bridged; `false` to suppress. Records the new state when
    /// admitting.
    async fn admit(&self, author: BareJid, kind: PepKind, summary: &str) -> bool {
        let mut guard = self.throttle.lock().await;
        let now = Instant::now();
        let cooldown = kind.cooldown();
        match guard.get(&(author.clone(), kind)) {
            Some(slot) if slot.last_summary == summary => {
                // Identical summary — suppress regardless of cooldown
                // so back-to-back re-publishes (avatar republish on
                // login etc.) don't generate duplicate entries.
                false
            }
            Some(slot) if now.duration_since(slot.last_at) < cooldown => false,
            _ => {
                guard.insert(
                    (author, kind),
                    ThrottleSlot {
                        last_at: now,
                        last_summary: summary.to_string(),
                    },
                );
                true
            }
        }
    }
}

impl Default for PepFeedBridge {
    fn default() -> Self {
        Self::new()
    }
}

// ── Summary rendering ───────────────────────────────────────────────

fn render_summary(kind: PepKind, item: &PubSubItem) -> Option<String> {
    let payload = item.payload.as_ref()?;
    match kind {
        PepKind::Mood => render_mood(payload),
        PepKind::Activity => render_activity(payload),
        PepKind::Tune => render_tune(payload),
        PepKind::Avatar => render_avatar(payload),
        PepKind::VCard => render_vcard(payload),
        // RSVPs render via `render_rsvp_summary` from `observe_rsvp`;
        // they never flow through PEP `observe`.
        PepKind::Rsvp => None,
    }
}

fn render_mood(payload: &Element) -> Option<String> {
    let mood = waddle_xmpp::xep::xep0107::parse_mood_element(payload)
        .ok()
        .flatten()?;
    let kind = mood.kind.as_element_name();
    let detail = mood
        .text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    Some(match detail {
        Some(text) => format!("is feeling {kind} — {text}"),
        None => format!("is feeling {kind}"),
    })
}

fn render_activity(payload: &Element) -> Option<String> {
    let activity = waddle_xmpp::xep::xep0108::parse_activity_element(payload)
        .ok()
        .flatten()?;
    let general = activity.general.as_element_name().replace('_', " ");
    let specific = activity
        .specific
        .as_ref()
        .map(|s| s.as_str().replace('_', " "));
    let detail = activity
        .text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let head = match specific.as_deref() {
        Some(specific) if specific != general => format!("is {general} ({specific})"),
        _ => format!("is {general}"),
    };
    Some(match detail {
        Some(text) => format!("{head} — {text}"),
        None => head,
    })
}

fn render_tune(payload: &Element) -> Option<String> {
    let tune = waddle_xmpp::xep::xep0118::parse_tune_element(payload).ok()?;
    match (tune.title.as_deref(), tune.artist.as_deref()) {
        (Some(title), Some(artist)) => Some(format!("is listening to {title} by {artist}")),
        (Some(title), None) => Some(format!("is listening to {title}")),
        (None, Some(artist)) => Some(format!("is listening to {artist}")),
        (None, None) => None,
    }
}

fn render_avatar(payload: &Element) -> Option<String> {
    let info = waddle_xmpp::xep::xep0084::parse_avatar_metadata(payload)?;
    Some(format!(
        "updated their avatar ({})",
        &info.id[..8.min(info.id.len())]
    ))
}

/// Render a one-line RSVP summary like "is going to Friday Game
/// Night". `partstat` chooses the verb; `NEEDS-ACTION` is treated
/// as a passive "is invited to" — the chat shouldn't normally
/// publish that as an RSVP, but if it ever does we surface it
/// without exploding.
fn render_rsvp_summary(partstat: waddle_xmpp_core::xcal::PartStat, event_label: &str) -> String {
    let verb = match partstat {
        waddle_xmpp_core::xcal::PartStat::Accepted => "is going to",
        waddle_xmpp_core::xcal::PartStat::Tentative => "might go to",
        waddle_xmpp_core::xcal::PartStat::Declined => "won't be at",
        waddle_xmpp_core::xcal::PartStat::NeedsAction => "is invited to",
    };
    format!("{verb} {event_label}")
}

/// Fetch the master VEVENT for `master_uid` from the community
/// events node. Returns `None` when the item isn't found or the
/// payload isn't a valid VCALENDAR (a race window when the event
/// was retracted between RSVP publish and bridge observation).
async fn lookup_master_event<S: PubSubStorage + ?Sized>(
    storage: &Arc<S>,
    community_jid: &BareJid,
    master_uid: &str,
) -> Option<waddle_xmpp_core::xcal::VEvent> {
    let item_ids = [master_uid.to_string()];
    let items = storage
        .get_items(
            community_jid,
            waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
            None,
            &item_ids,
        )
        .await
        .ok()?;
    let stored = items.into_iter().next()?;
    let pubsub_item = stored.to_pubsub_item();
    let payload = pubsub_item.payload?;
    waddle_xmpp_core::xcal::parse_vcalendar_event(master_uid, &payload)
}

fn short_uid(uid: &str) -> &str {
    &uid[..12.min(uid.len())]
}

fn render_vcard(payload: &Element) -> Option<String> {
    let vcard = waddle_xmpp::xep::xep0292::parse_vcard4(payload);
    if vcard.full_name.is_none()
        && vcard.nickname.is_none()
        && vcard.note.is_none()
        && vcard.org.is_none()
        && vcard.title.is_none()
    {
        return None;
    }
    Some("updated their profile".to_string())
}

// ── Feed-entry builder ──────────────────────────────────────────────

fn build_bridge_entry(item_id: &str, kind: PepKind, author: &BareJid, body: &str) -> Element {
    let mut entry = Element::builder("entry", waddle_xmpp_core::xep0472::NS_SOCIAL_FEED).build();

    let mut id_el = Element::builder("id", waddle_xmpp_core::xep0472::NS_SOCIAL_FEED).build();
    id_el.append_text_node(item_id);
    entry.append_child(id_el);

    let mut author_el =
        Element::builder("author", waddle_xmpp_core::xep0472::NS_SOCIAL_FEED).build();
    author_el.append_text_node(author.to_string());
    entry.append_child(author_el);

    let mut body_el = Element::builder("body", waddle_xmpp_core::xep0472::NS_SOCIAL_FEED).build();
    body_el.append_text_node(body);
    entry.append_child(body_el);

    let mut published_el =
        Element::builder("published", waddle_xmpp_core::xep0472::NS_SOCIAL_FEED).build();
    published_el.append_text_node(chrono::Utc::now().to_rfc3339());
    entry.append_child(published_el);

    let source = Element::builder("source", NS_FEED_SOURCE)
        .attr("kind", kind.as_str())
        .build();
    entry.append_child(source);

    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn payload(xml: &str) -> PubSubItem {
        let element: Element = xml.parse().expect("valid xml");
        PubSubItem {
            id: Some("test".to_string()),
            publisher: None,
            payload: Some(element),
        }
    }

    #[test]
    fn mood_summary_renders_kind_name() {
        let item = payload("<mood xmlns='http://jabber.org/protocol/mood'><happy/></mood>");
        let summary = render_summary(PepKind::Mood, &item).expect("renders");
        assert_eq!(summary, "is feeling happy");
    }

    #[test]
    fn activity_summary_uses_general_kind() {
        let item =
            payload("<activity xmlns='http://jabber.org/protocol/activity'><working/></activity>");
        let summary = render_summary(PepKind::Activity, &item).expect("renders");
        assert_eq!(summary, "is working");
    }

    #[test]
    fn activity_summary_humanises_underscored_kinds() {
        let item = payload(
            "<activity xmlns='http://jabber.org/protocol/activity'><doing_chores/></activity>",
        );
        let summary = render_summary(PepKind::Activity, &item).expect("renders");
        assert_eq!(summary, "is doing chores");
    }

    #[test]
    fn mood_summary_appends_user_text() {
        let item = payload(
            "<mood xmlns='http://jabber.org/protocol/mood'>\
                <excited/>\
                <text>Friday tournament tonight!</text>\
            </mood>",
        );
        let summary = render_summary(PepKind::Mood, &item).expect("renders");
        assert_eq!(summary, "is feeling excited — Friday tournament tonight!");
    }

    #[test]
    fn activity_summary_includes_specific_and_text() {
        let item = payload(
            "<activity xmlns='http://jabber.org/protocol/activity'>\
                <working><coding/></working>\
                <text>migrating the calendar module to xCal</text>\
            </activity>",
        );
        let summary = render_summary(PepKind::Activity, &item).expect("renders");
        assert_eq!(
            summary,
            "is working (coding) — migrating the calendar module to xCal"
        );
    }

    #[test]
    fn rsvp_summary_renders_each_partstat() {
        use waddle_xmpp_core::xcal::PartStat;
        assert_eq!(
            render_rsvp_summary(PartStat::Accepted, "Friday Game Night"),
            "is going to Friday Game Night"
        );
        assert_eq!(
            render_rsvp_summary(PartStat::Tentative, "Friday Game Night"),
            "might go to Friday Game Night"
        );
        assert_eq!(
            render_rsvp_summary(PartStat::Declined, "Friday Game Night"),
            "won't be at Friday Game Night"
        );
        assert_eq!(
            render_rsvp_summary(PartStat::NeedsAction, "Friday Game Night"),
            "is invited to Friday Game Night"
        );
    }

    #[test]
    fn activity_summary_drops_redundant_specific_equal_to_general() {
        // Defensive: if a client somehow emits the general name as
        // the specific (unusual but seen in the wild), don't repeat.
        let item = payload(
            "<activity xmlns='http://jabber.org/protocol/activity'>\
                <working><working/></working>\
            </activity>",
        );
        let summary = render_summary(PepKind::Activity, &item).expect("renders");
        assert_eq!(summary, "is working");
    }

    #[test]
    fn tune_summary_includes_title_and_artist() {
        let item = payload(
            "<tune xmlns='http://jabber.org/protocol/tune'>\
                <title>Africa</title>\
                <artist>Toto</artist>\
            </tune>",
        );
        let summary = render_summary(PepKind::Tune, &item).expect("renders");
        assert_eq!(summary, "is listening to Africa by Toto");
    }

    #[test]
    fn tune_summary_renders_with_title_only() {
        let item =
            payload("<tune xmlns='http://jabber.org/protocol/tune'><title>Africa</title></tune>");
        let summary = render_summary(PepKind::Tune, &item).expect("renders");
        assert_eq!(summary, "is listening to Africa");
    }

    #[test]
    fn tune_summary_empty_when_no_track_info() {
        let item =
            payload("<tune xmlns='http://jabber.org/protocol/tune'><length>180</length></tune>");
        assert_eq!(render_summary(PepKind::Tune, &item), None);
    }

    #[test]
    fn avatar_summary_short_hashes_id() {
        let item = payload(
            "<metadata xmlns='urn:xmpp:avatar:metadata'>\
                <info id='abcdef1234567890' bytes='100' type='image/png'/>\
            </metadata>",
        );
        let summary = render_summary(PepKind::Avatar, &item).expect("renders");
        assert_eq!(summary, "updated their avatar (abcdef12)");
    }

    #[test]
    fn vcard_summary_requires_a_meaningful_field() {
        let empty = payload("<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'/>");
        assert_eq!(render_summary(PepKind::VCard, &empty), None);

        let with_name = payload(
            "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>\
                <fn><text>Alice</text></fn>\
            </vcard>",
        );
        let summary = render_summary(PepKind::VCard, &with_name).expect("renders");
        assert_eq!(summary, "updated their profile");
    }

    #[tokio::test]
    async fn throttle_suppresses_identical_summary_back_to_back() {
        let bridge = PepFeedBridge {
            enabled: true,
            throttle: Mutex::new(HashMap::new()),
            rsvp_throttle: Mutex::new(HashMap::new()),
        };
        let author = bare("alice@example.com");
        assert!(
            bridge
                .admit(author.clone(), PepKind::Mood, "is feeling happy")
                .await
        );
        // Same summary — suppressed even though cooldown hasn't fired.
        assert!(
            !bridge
                .admit(author.clone(), PepKind::Mood, "is feeling happy")
                .await
        );
    }

    #[tokio::test]
    async fn throttle_admits_different_summary_after_first() {
        let bridge = PepFeedBridge {
            enabled: true,
            throttle: Mutex::new(HashMap::new()),
            rsvp_throttle: Mutex::new(HashMap::new()),
        };
        let author = bare("alice@example.com");
        assert!(
            bridge
                .admit(author.clone(), PepKind::Mood, "is feeling happy")
                .await
        );
        // Different summary within cooldown — still suppressed (cooldown gates per-kind).
        assert!(
            !bridge
                .admit(author.clone(), PepKind::Mood, "is feeling sad")
                .await
        );
    }

    #[tokio::test]
    async fn throttle_isolates_per_user_and_per_kind() {
        let bridge = PepFeedBridge {
            enabled: true,
            throttle: Mutex::new(HashMap::new()),
            rsvp_throttle: Mutex::new(HashMap::new()),
        };
        let alice = bare("alice@example.com");
        let bob = bare("bob@example.com");
        assert!(
            bridge
                .admit(alice.clone(), PepKind::Mood, "is feeling happy")
                .await
        );
        // Different user — admitted.
        assert!(
            bridge
                .admit(bob.clone(), PepKind::Mood, "is feeling happy")
                .await
        );
        // Same user, different kind — admitted.
        assert!(
            bridge
                .admit(alice.clone(), PepKind::Tune, "is listening to X")
                .await
        );
    }

    #[test]
    fn from_node_recognises_all_bridged_namespaces() {
        assert_eq!(PepKind::from_node(NS_MOOD), Some(PepKind::Mood));
        assert_eq!(PepKind::from_node(NS_ACTIVITY), Some(PepKind::Activity));
        assert_eq!(PepKind::from_node(NS_TUNE), Some(PepKind::Tune));
        assert_eq!(
            PepKind::from_node(NS_AVATAR_METADATA),
            Some(PepKind::Avatar)
        );
        assert_eq!(PepKind::from_node(NS_VCARD4), Some(PepKind::VCard));
        assert_eq!(PepKind::from_node("urn:xmpp:avatar:data"), None);
    }

    #[test]
    fn bridge_entry_carries_author_body_and_source_kind() {
        let entry = build_bridge_entry(
            "pep-1",
            PepKind::Mood,
            &bare("alice@example.com"),
            "is feeling happy",
        );
        let xml = String::from(&entry);
        assert!(
            xml.contains("<author>alice@example.com</author>"),
            "author missing: {xml}"
        );
        assert!(
            xml.contains("<body>is feeling happy</body>"),
            "body missing: {xml}"
        );
        assert!(
            xml.contains("kind=\"mood\"") || xml.contains("kind='mood'"),
            "source kind missing: {xml}"
        );
    }
}
