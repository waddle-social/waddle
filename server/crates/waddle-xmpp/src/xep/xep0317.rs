//! XEP-0317: Hats
//!
//! Provides role badges/labels for MUC room occupants. Hats are visual
//! indicators showing roles like "Admin", "Moderator", "Bot", etc.
//!
//! ## XML Format
//!
//! In MUC presence:
//! ```xml
//! <presence from='room@muc.example.com/nick'>
//!   <x xmlns='http://jabber.org/protocol/muc#user'>
//!     <item affiliation='admin' role='moderator'/>
//!   </x>
//!   <hats xmlns='urn:xmpp:hats:0'>
//!     <hat title='Admin' uri='urn:xmpp:hats:admin'/>
//!     <hat title='Bot' uri='urn:xmpp:hats:bot'/>
//!   </hats>
//! </presence>
//! ```
//!
//! ## Use Cases
//!
//! - Show role badges next to usernames in chat (like Discord roles)
//! - Distinguish admins, moderators, bots from regular users
//! - Custom room-specific roles/badges
//!
//! ## Server Behavior
//!
//! The MUC service adds `<hats/>` to occupant presence based on their
//! affiliations and roles. The server may also allow custom hat assignment.

use minidom::Element;
use xmpp_parsers::presence::Presence;

/// Namespace for XEP-0317 Hats.
pub const NS_HATS: &str = "urn:xmpp:hats:0";

/// Well-known hat URIs for common roles.
pub mod well_known {
    /// Server/community administrator.
    pub const ADMIN: &str = "urn:xmpp:hats:admin";
    /// Channel/room moderator.
    pub const MODERATOR: &str = "urn:xmpp:hats:moderator";
    /// Room owner.
    pub const OWNER: &str = "urn:xmpp:hats:owner";
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

    /// Create an admin hat.
    pub fn admin() -> Self {
        Self::new("Admin", well_known::ADMIN)
    }

    /// Create a moderator hat.
    pub fn moderator() -> Self {
        Self::new("Moderator", well_known::MODERATOR)
    }

    /// Create an owner hat.
    pub fn owner() -> Self {
        Self::new("Owner", well_known::OWNER)
    }

    /// Create a bot hat.
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

    /// Check if the occupant is an admin.
    pub fn is_admin(&self) -> bool {
        self.has_uri(well_known::ADMIN)
    }

    /// Check if the occupant is a moderator.
    pub fn is_moderator(&self) -> bool {
        self.has_uri(well_known::MODERATOR)
    }

    /// Check if the occupant is a bot.
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
    presence.payloads.iter().any(|e| is_hats_element(e))
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

/// Generate a hat set from a MUC affiliation.
///
/// Maps standard MUC affiliations to well-known hat types.
pub fn hats_from_affiliation(affiliation: &str) -> HatSet {
    let mut set = HatSet::new();
    match affiliation {
        "owner" => set = set.with_hat(Hat::owner()),
        "admin" => set = set.with_hat(Hat::admin()),
        "member" => {} // No hat for regular members
        "none" => {}
        "outcast" => {}
        _ => {}
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_hats_element() {
        let elem = Element::builder("hats", NS_HATS).build();
        assert!(is_hats_element(&elem));

        let wrong = Element::builder("hats", "jabber:client").build();
        assert!(!is_hats_element(&wrong));
    }

    #[test]
    fn test_build_and_parse() {
        let set = HatSet::new()
            .with_hat(Hat::admin())
            .with_hat(Hat::new("Custom", "urn:example:custom"));

        let elem = build_hats_element(&set);
        assert_eq!(elem.name(), "hats");
        assert_eq!(elem.ns(), NS_HATS);

        let parsed = parse_hats_element(&elem);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.hats[0].title, "Admin");
        assert_eq!(parsed.hats[0].uri, well_known::ADMIN);
        assert_eq!(parsed.hats[1].title, "Custom");
    }

    #[test]
    fn test_parse_from_presence() {
        let xml = "<presence xmlns='jabber:client'>\
                    <hats xmlns='urn:xmpp:hats:0'>\
                      <hat title='Moderator' uri='urn:xmpp:hats:moderator'/>\
                      <hat title='Bot' uri='urn:xmpp:hats:bot'/>\
                    </hats>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        let set = extract_hats_from_presence(&presence).expect("has hats");
        assert_eq!(set.len(), 2);
        assert!(set.is_moderator());
        assert!(set.is_bot());
        assert!(!set.is_admin());
    }

    #[test]
    fn test_extract_absent() {
        let xml = "<presence xmlns='jabber:client'/>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");
        assert!(extract_hats_from_presence(&presence).is_none());
    }

    #[test]
    fn test_empty_hats() {
        let set = HatSet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn test_set_hats() {
        let xml = "<presence xmlns='jabber:client'/>";
        let mut presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        let set = HatSet::new().with_hat(Hat::admin());
        set_hats(&mut presence, &set);
        assert!(has_hats(&presence));

        // Replace
        let set2 = HatSet::new().with_hat(Hat::moderator());
        set_hats(&mut presence, &set2);
        let extracted = extract_hats_from_presence(&presence).expect("has hats");
        assert_eq!(extracted.len(), 1);
        assert!(extracted.is_moderator());
    }

    #[test]
    fn test_strip_hats() {
        let xml = "<presence xmlns='jabber:client'>\
                    <hats xmlns='urn:xmpp:hats:0'>\
                      <hat title='Admin' uri='urn:xmpp:hats:admin'/>\
                    </hats>\
                    </presence>";
        let mut presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        strip_hats(&mut presence);
        assert!(!has_hats(&presence));
    }

    #[test]
    fn test_hat_carrier_trait() {
        let xml = "<presence xmlns='jabber:client'>\
                    <hats xmlns='urn:xmpp:hats:0'>\
                      <hat title='Owner' uri='urn:xmpp:hats:owner'/>\
                    </hats>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        assert!(presence.has_hats());
        let set = presence.hat_set().expect("has hats");
        assert!(set.has_uri(well_known::OWNER));
    }

    #[test]
    fn test_hat_display() {
        let hat = Hat::admin();
        assert_eq!(hat.to_string(), "Admin");
    }

    #[test]
    fn test_hat_constructors() {
        assert_eq!(Hat::admin().uri, well_known::ADMIN);
        assert_eq!(Hat::moderator().uri, well_known::MODERATOR);
        assert_eq!(Hat::owner().uri, well_known::OWNER);
        assert_eq!(Hat::bot().uri, well_known::BOT);
    }

    #[test]
    fn test_titles() {
        let set = HatSet::new().with_hat(Hat::admin()).with_hat(Hat::bot());
        assert_eq!(set.titles(), vec!["Admin", "Bot"]);
    }

    #[test]
    fn test_hats_from_affiliation() {
        assert!(hats_from_affiliation("owner").has_uri(well_known::OWNER));
        assert!(hats_from_affiliation("admin").has_uri(well_known::ADMIN));
        assert!(hats_from_affiliation("member").is_empty());
        assert!(hats_from_affiliation("none").is_empty());
    }

    #[test]
    fn test_skip_hat_missing_attrs() {
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
    fn test_set_empty_hats_removes_element() {
        let xml = "<presence xmlns='jabber:client'>\
                    <hats xmlns='urn:xmpp:hats:0'>\
                      <hat title='Admin' uri='urn:xmpp:hats:admin'/>\
                    </hats>\
                    </presence>";
        let mut presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        set_hats(&mut presence, &HatSet::new());
        assert!(!has_hats(&presence));
    }
}
