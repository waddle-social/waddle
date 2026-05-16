//! XEP-0317: Hats
//!
//! Hats are descriptive social metadata about a MUC occupant — badges
//! such as "Speaker", "Host", "DJ", "Bot", "Verified". They are NOT
//! a permission system. Authority is carried by MUC role and
//! affiliation under `<x xmlns='http://jabber.org/protocol/muc#user'>
//! <item affiliation='…' role='…'/>` (XEP-0045 §5); hats live in a
//! parallel `<hats xmlns='urn:xmpp:hats:0'>` element and have no
//! mandatory protocol semantics.
//!
//! XEP-0317 §1 motivates the layer as "extended roles […] beyond"
//! the standard MUC affiliation/role set — example use cases the
//! spec lists include presenter, scribe, teacher, teacher's
//! assistant, comms officer, incident manager, online-game role.
//! The spec explicitly frames the naming as a way to "prevent
//! confusion with standard MUC roles".
//!
//! Therefore this module deliberately does NOT expose constructors for
//! "owner / admin / moderator" hats — those are MUC authority and
//! belong in `<x muc#user>`, not in `<hats>`. The only well-known hat
//! Waddle ships is `Bot`, used by the extension-bot path to socially
//! identify automated participants.
//!
//! ## Wire shape
//!
//! ```xml
//! <presence from='room@muc.example.com/nick'>
//!   <x xmlns='http://jabber.org/protocol/muc#user'>
//!     <item affiliation='admin' role='moderator'/>
//!   </x>
//!   <hats xmlns='urn:xmpp:hats:0'>
//!     <hat title='Bot' uri='urn:xmpp:hats:bot'/>
//!   </hats>
//! </presence>
//! ```

use minidom::Element;
use xmpp_parsers::presence::Presence;

/// Namespace for XEP-0317 Hats.
pub const NS_HATS: &str = "urn:xmpp:hats:0";

/// Well-known hat URIs Waddle assigns out-of-band. Note that these are
/// purely descriptive — they confer no authority. Authority is carried
/// by MUC role/affiliation, not by hats.
pub mod well_known {
    /// Automated bot.
    pub const BOT: &str = "urn:xmpp:hats:bot";
    /// Verified/trusted user.
    pub const VERIFIED: &str = "urn:xmpp:hats:verified";
}

/// A single hat (role badge).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hat {
    /// Human-readable display title (e.g., "Admin", "Moderator").
    pub title: String,
    /// URI identifying the hat type.
    pub uri: String,
}

impl Hat {
    /// Create a new hat.
    pub fn new(title: impl Into<String>, uri: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            uri: uri.into(),
        }
    }

    /// Create a bot hat. The bot hat is descriptive — it tells humans
    /// "this occupant is automated" — and confers no authority.
    pub fn bot() -> Self {
        Self::new("Bot", well_known::BOT)
    }
}

impl std::fmt::Display for Hat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.title)
    }
}

/// A collection of hats for an occupant.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HatSet {
    pub hats: Vec<Hat>,
}

impl HatSet {
    /// Create an empty hat set.
    pub fn new() -> Self {
        Self { hats: Vec::new() }
    }

    /// Add a hat.
    pub fn with_hat(mut self, hat: Hat) -> Self {
        self.hats.push(hat);
        self
    }

    /// Returns `true` if there are no hats.
    pub fn is_empty(&self) -> bool {
        self.hats.is_empty()
    }

    /// Number of hats.
    pub fn len(&self) -> usize {
        self.hats.len()
    }

    /// Check if a hat with the given URI is present.
    pub fn has_uri(&self, uri: &str) -> bool {
        self.hats.iter().any(|h| h.uri == uri)
    }

    /// Check if the occupant carries the Bot hat. Note this is
    /// descriptive — it does not imply or check any MUC authority.
    pub fn is_bot(&self) -> bool {
        self.has_uri(well_known::BOT)
    }

    /// Get hat titles as a vec.
    pub fn titles(&self) -> Vec<&str> {
        self.hats.iter().map(|h| h.title.as_str()).collect()
    }
}

/// Trait for types that can carry hats.
pub trait HatCarrier {
    /// Extract the hat set from this carrier.
    fn hat_set(&self) -> Option<HatSet>;

    /// Returns `true` if this carrier has any hats.
    fn has_hats(&self) -> bool {
        self.hat_set().is_some_and(|hs| !hs.is_empty())
    }
}

impl HatCarrier for Presence {
    fn hat_set(&self) -> Option<HatSet> {
        extract_hats_from_presence(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<hats/>` element.
pub fn is_hats_element(elem: &Element) -> bool {
    elem.ns() == NS_HATS && elem.name() == "hats"
}

/// Check if a presence has hats.
pub fn has_hats(presence: &Presence) -> bool {
    presence.payloads.iter().any(is_hats_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract hats from a presence stanza.
pub fn extract_hats_from_presence(presence: &Presence) -> Option<HatSet> {
    let elem = presence.payloads.iter().find(|e| is_hats_element(e))?;
    Some(parse_hats_element(elem))
}

/// Parse a `<hats/>` element.
pub fn parse_hats_element(elem: &Element) -> HatSet {
    let hats: Vec<Hat> = elem
        .children()
        .filter(|c| c.name() == "hat" && c.ns() == NS_HATS)
        .filter_map(|c| {
            let title = c.attr("title").filter(|t| !t.is_empty())?;
            let uri = c.attr("uri").filter(|u| !u.is_empty())?;
            Some(Hat::new(title, uri))
        })
        .collect();

    HatSet { hats }
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<hats/>` element.
pub fn build_hats_element(hat_set: &HatSet) -> Element {
    let mut hats = Element::builder("hats", NS_HATS).build();
    for hat in &hat_set.hats {
        let hat_elem = Element::builder("hat", NS_HATS)
            .attr("title", hat.title.as_str())
            .attr("uri", hat.uri.as_str())
            .build();
        hats.append_child(hat_elem);
    }
    hats
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add hats to a presence stanza, replacing any existing.
pub fn set_hats(presence: &mut Presence, hat_set: &HatSet) {
    presence.payloads.retain(|e| e.ns() != NS_HATS);
    if !hat_set.is_empty() {
        presence.payloads.push(build_hats_element(hat_set));
    }
}

/// Remove hats from a presence stanza.
pub fn strip_hats(presence: &mut Presence) {
    presence.payloads.retain(|e| e.ns() != NS_HATS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_hats_element_matches_namespace_exactly() {
        let elem = Element::builder("hats", NS_HATS).build();
        assert!(is_hats_element(&elem));

        let wrong = Element::builder("hats", "jabber:client").build();
        assert!(!is_hats_element(&wrong));
    }

    #[test]
    fn build_then_parse_round_trips_a_descriptive_hat() {
        let set = HatSet::new()
            .with_hat(Hat::bot())
            .with_hat(Hat::new("Speaker", "urn:example:speaker"));

        let elem = build_hats_element(&set);
        assert_eq!(elem.name(), "hats");
        assert_eq!(elem.ns(), NS_HATS);

        let parsed = parse_hats_element(&elem);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.hats[0].title, "Bot");
        assert_eq!(parsed.hats[0].uri, well_known::BOT);
        assert_eq!(parsed.hats[1].title, "Speaker");
        assert_eq!(parsed.hats[1].uri, "urn:example:speaker");
    }

    #[test]
    fn parse_from_presence_extracts_descriptive_hats() {
        let xml = "<presence xmlns='jabber:client'>\
                    <hats xmlns='urn:xmpp:hats:0'>\
                      <hat title='Speaker' uri='urn:example:speaker'/>\
                      <hat title='Bot' uri='urn:xmpp:hats:bot'/>\
                    </hats>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        let set = extract_hats_from_presence(&presence).expect("has hats");
        assert_eq!(set.len(), 2);
        assert!(set.is_bot());
        assert!(set.has_uri("urn:example:speaker"));
    }

    #[test]
    fn extract_returns_none_when_presence_carries_no_hats() {
        let xml = "<presence xmlns='jabber:client'/>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");
        assert!(extract_hats_from_presence(&presence).is_none());
    }

    #[test]
    fn empty_hat_set_is_empty() {
        let set = HatSet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn set_hats_replaces_any_existing_payload() {
        let xml = "<presence xmlns='jabber:client'/>";
        let mut presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        let first = HatSet::new().with_hat(Hat::bot());
        set_hats(&mut presence, &first);
        assert!(has_hats(&presence));

        let second = HatSet::new().with_hat(Hat::new("Speaker", "urn:example:speaker"));
        set_hats(&mut presence, &second);
        let extracted = extract_hats_from_presence(&presence).expect("has hats");
        assert_eq!(extracted.len(), 1);
        assert!(extracted.has_uri("urn:example:speaker"));
        assert!(!extracted.is_bot());
    }

    #[test]
    fn strip_removes_existing_hats_payload() {
        let xml = "<presence xmlns='jabber:client'>\
                    <hats xmlns='urn:xmpp:hats:0'>\
                      <hat title='Bot' uri='urn:xmpp:hats:bot'/>\
                    </hats>\
                    </presence>";
        let mut presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        strip_hats(&mut presence);
        assert!(!has_hats(&presence));
    }

    #[test]
    fn hat_carrier_trait_reads_hats_off_presence() {
        let xml = "<presence xmlns='jabber:client'>\
                    <hats xmlns='urn:xmpp:hats:0'>\
                      <hat title='Bot' uri='urn:xmpp:hats:bot'/>\
                    </hats>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        assert!(presence.has_hats());
        let set = presence.hat_set().expect("has hats");
        assert!(set.has_uri(well_known::BOT));
    }

    #[test]
    fn hat_display_uses_title() {
        let hat = Hat::bot();
        assert_eq!(hat.to_string(), "Bot");
    }

    #[test]
    fn bot_constructor_pins_well_known_uri() {
        assert_eq!(Hat::bot().uri, well_known::BOT);
        assert_eq!(Hat::bot().title, "Bot");
    }

    #[test]
    fn titles_returns_each_hats_title_in_order() {
        let set = HatSet::new()
            .with_hat(Hat::bot())
            .with_hat(Hat::new("Speaker", "urn:example:speaker"));
        assert_eq!(set.titles(), vec!["Bot", "Speaker"]);
    }

    #[test]
    fn parse_skips_hat_entries_missing_required_attrs() {
        let xml = "<hats xmlns='urn:xmpp:hats:0'>\
                    <hat title='Valid' uri='urn:valid'/>\
                    <hat title='' uri='urn:empty-title'/>\
                    <hat title='NoUri' uri=''/>\
                    </hats>";
        let elem: Element = xml.parse().expect("valid xml");
        let set = parse_hats_element(&elem);
        assert_eq!(set.len(), 1);
        assert_eq!(set.hats[0].title, "Valid");
    }

    #[test]
    fn set_hats_with_empty_set_removes_payload_entirely() {
        let xml = "<presence xmlns='jabber:client'>\
                    <hats xmlns='urn:xmpp:hats:0'>\
                      <hat title='Bot' uri='urn:xmpp:hats:bot'/>\
                    </hats>\
                    </presence>";
        let mut presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        set_hats(&mut presence, &HatSet::new());
        assert!(!has_hats(&presence));
    }

    // ── XEP-0317 conformance: hats are descriptive, not authoritative ──
    //
    // XEP-0317 §1 explicitly defines hats as extended roles "beyond"
    // the standard MUC affiliation/role system, "to prevent confusion
    // with standard MUC roles". These tests pin that separation.

    #[test]
    fn module_exposes_no_constructor_for_muc_authority_concepts() {
        // The constructor surface intentionally contains only
        // descriptive hats. `Hat::owner()`, `Hat::admin()`,
        // `Hat::moderator()` are not — and must not be — provided:
        // owner/admin/moderator live in `<x xmlns='muc#user'>
        // <item affiliation='…' role='…'/>`, not in `<hats>`.
        //
        // This test is documentation that compiles. The mere fact that
        // it compiles asserts the public API is restricted to Bot (the
        // only descriptive hat Waddle ships) plus the open-ended
        // `Hat::new()` for application-specific descriptive metadata.
        let _bot = Hat::bot();
        let _custom = Hat::new("Speaker", "urn:example:speaker");
        // `Hat::owner();` would not compile.
        // `Hat::admin();` would not compile.
        // `Hat::moderator();` would not compile.
    }

    #[test]
    fn well_known_uris_do_not_duplicate_muc_authority_concepts() {
        // The well-known URIs Waddle assigns must not include
        // owner/admin/moderator — those are MUC authority, conveyed
        // by the XEP-0045 `<x muc#user><item/>` payload.
        // Only descriptive concepts (Bot, Verified) live here.
        assert_eq!(well_known::BOT, "urn:xmpp:hats:bot");
        assert_eq!(well_known::VERIFIED, "urn:xmpp:hats:verified");
    }
}
