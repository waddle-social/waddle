//! MUC Admin Operations (XEP-0045 §10)
//!
//! Implements IQ-based admin operations for Multi-User Chat rooms:
//! - Getting affiliation lists (§10.1)
//! - Setting affiliations (§10.2)
//! - Kicking occupants (via role change)
//!
//! ## Namespaces
//! - `http://jabber.org/protocol/muc#admin` - Admin operations
//! - `http://jabber.org/protocol/muc#owner` - Owner operations (config, destroy)

use jid::{BareJid, Jid};
use minidom::Element;
use tracing::{debug, instrument};
use xmpp_parsers::iq::{Iq, IqType};

use crate::types::{Affiliation, Role};
use crate::XmppError;

/// Namespace for MUC admin protocol.
pub const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";

/// Namespace for MUC owner protocol.
pub const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";

/// Check if an IQ is a MUC admin query (get affiliation list).
///
/// Returns true if the IQ is a 'get' request to the muc#admin namespace.
pub fn is_muc_admin_get(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem) if elem.is("query", NS_MUC_ADMIN))
}

/// Check if an IQ is a MUC admin set (modify affiliations/roles).
///
/// Returns true if the IQ is a 'set' request to the muc#admin namespace.
pub fn is_muc_admin_set(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Set(elem) if elem.is("query", NS_MUC_ADMIN))
}

/// Check if an IQ is a MUC owner query (get room config).
pub fn is_muc_owner_get(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem) if elem.is("query", NS_MUC_OWNER))
}

/// Check if an IQ is a MUC owner set (set room config or destroy).
pub fn is_muc_owner_set(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Set(elem) if elem.is("query", NS_MUC_OWNER))
}

/// Check if an IQ is directed at a MUC room (for routing purposes).
pub fn is_muc_admin_iq(iq: &Iq, muc_domain: &str) -> bool {
    // Check if the 'to' JID is in the MUC domain
    let to_jid = match &iq.to {
        Some(jid) => jid,
        None => return false,
    };

    let bare_jid = to_jid.to_bare();
    bare_jid.domain().as_str() == muc_domain
        && (is_muc_admin_get(iq) || is_muc_admin_set(iq) || is_muc_owner_get(iq) || is_muc_owner_set(iq))
}

/// Parsed admin query request.
#[derive(Debug)]
pub struct AdminQuery {
    /// The room JID being queried
    pub room_jid: BareJid,
    /// The IQ ID for response correlation
    pub iq_id: String,
    /// The sender's JID
    pub from: Jid,
    /// The requested affiliation filter (for GET) or items to modify (for SET)
    pub items: Vec<AdminItem>,
    /// Whether this is a 'get' or 'set' request
    pub is_get: bool,
}

/// An item in an admin query (JID + affiliation/role).
#[derive(Debug, Clone)]
pub struct AdminItem {
    /// The JID to query/modify
    pub jid: Option<BareJid>,
    /// Nickname (for role changes of current occupants)
    pub nick: Option<String>,
    /// Requested/current affiliation
    pub affiliation: Option<Affiliation>,
    /// Requested/current role
    pub role: Option<Role>,
    /// Reason for the action
    pub reason: Option<String>,
}

/// Parse a MUC admin IQ request.
///
/// Extracts the room JID, items to query/modify, and determines
/// whether this is a get or set operation.
#[instrument(skip(iq), fields(iq_id = %iq.id))]
pub fn parse_admin_query(iq: &Iq, muc_domain: &str) -> Result<AdminQuery, XmppError> {
    // Get the room JID from the 'to' attribute
    let room_jid = iq.to.as_ref()
        .ok_or_else(|| XmppError::bad_request(Some("Missing 'to' attribute".into())))?
        .to_bare();

    // Verify it's a MUC room JID
    if room_jid.domain().as_str() != muc_domain {
        return Err(XmppError::bad_request(Some(format!(
            "IQ to {} is not a MUC room", room_jid
        ))));
    }

    // Get the sender's JID
    let from = iq.from.clone()
        .ok_or_else(|| XmppError::bad_request(Some("Missing 'from' attribute".into())))?;

    // Determine if this is a get or set request
    let (is_get, query_elem) = match &iq.payload {
        IqType::Get(elem) => (true, elem),
        IqType::Set(elem) => (false, elem),
        _ => return Err(XmppError::bad_request(Some("Expected get or set IQ".into()))),
    };

    // Parse items from the query
    let items = parse_admin_items(query_elem)?;

    debug!(
        room = %room_jid,
        is_get = is_get,
        item_count = items.len(),
        "Parsed MUC admin query"
    );

    Ok(AdminQuery {
        room_jid,
        iq_id: iq.id.clone(),
        from,
        items,
        is_get,
    })
}

/// Parse item elements from an admin query.
fn parse_admin_items(query: &Element) -> Result<Vec<AdminItem>, XmppError> {
    let mut items = Vec::new();

    for child in query.children() {
        if child.name() == "item" {
            let jid = child.attr("jid")
                .and_then(|s| s.parse::<BareJid>().ok());

            let nick = child.attr("nick").map(String::from);

            let affiliation = child.attr("affiliation")
                .map(parse_muc_affiliation)
                .transpose()?;

            let role = child.attr("role")
                .map(parse_muc_role)
                .transpose()?;

            let reason = child.get_child("reason", NS_MUC_ADMIN)
                .or_else(|| child.get_child("reason", ""))
                .map(|r| r.text());

            items.push(AdminItem {
                jid,
                nick,
                affiliation,
                role,
                reason,
            });
        }
    }

    Ok(items)
}

/// Parse an affiliation string to our internal type.
fn parse_muc_affiliation(s: &str) -> Result<Affiliation, XmppError> {
    match s {
        "owner" => Ok(Affiliation::Owner),
        "admin" => Ok(Affiliation::Admin),
        "member" => Ok(Affiliation::Member),
        "none" => Ok(Affiliation::None),
        "outcast" => Ok(Affiliation::Outcast),
        _ => Err(XmppError::bad_request(Some(format!(
            "Unknown affiliation: {}", s
        )))),
    }
}

/// Parse a role string to our internal type.
fn parse_muc_role(s: &str) -> Result<Role, XmppError> {
    match s {
        "moderator" => Ok(Role::Moderator),
        "participant" => Ok(Role::Participant),
        "visitor" => Ok(Role::Visitor),
        "none" => Ok(Role::None),
        _ => Err(XmppError::bad_request(Some(format!(
            "Unknown role: {}", s
        )))),
    }
}

/// Convert internal Affiliation to string for XML.
pub fn affiliation_to_str(aff: Affiliation) -> &'static str {
    match aff {
        Affiliation::Owner => "owner",
        Affiliation::Admin => "admin",
        Affiliation::Member => "member",
        Affiliation::None => "none",
        Affiliation::Outcast => "outcast",
    }
}

/// Convert internal Role to string for XML.
pub fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::Moderator => "moderator",
        Role::Participant => "participant",
        Role::Visitor => "visitor",
        Role::None => "none",
    }
}

/// Build an admin query result response.
///
/// Creates an IQ result with a list of JID+affiliation items.
pub fn build_admin_result(
    iq_id: &str,
    from_room_jid: &BareJid,
    to_jid: &Jid,
    items: &[(BareJid, Affiliation)],
) -> Iq {
    let mut query = Element::builder("query", NS_MUC_ADMIN);

    for (jid, affiliation) in items {
        let item = Element::builder("item", NS_MUC_ADMIN)
            .attr("jid", jid.to_string())
            .attr("affiliation", affiliation_to_str(*affiliation))
            .build();
        query = query.append(item);
    }

    Iq {
        from: Some(Jid::from(from_room_jid.clone())),
        to: Some(to_jid.clone()),
        id: iq_id.to_string(),
        payload: IqType::Result(Some(query.build())),
    }
}

/// Build an empty admin set result (success).
pub fn build_admin_set_result(
    iq_id: &str,
    from_room_jid: &BareJid,
    to_jid: &Jid,
) -> Iq {
    Iq {
        from: Some(Jid::from(from_room_jid.clone())),
        to: Some(to_jid.clone()),
        id: iq_id.to_string(),
        payload: IqType::Result(None),
    }
}

/// Result of an affiliation change operation.
#[derive(Debug)]
pub struct AffiliationChangeResult {
    /// JID that was modified
    pub jid: BareJid,
    /// New affiliation
    pub new_affiliation: Affiliation,
    /// Whether occupants need presence updates
    pub needs_presence_update: bool,
    /// Nick of affected occupant (if currently in room)
    pub affected_nick: Option<String>,
}

/// Result of a role change (kick) operation.
#[derive(Debug)]
pub struct RoleChangeResult {
    /// Nick of the affected occupant
    pub nick: String,
    /// New role (None = kicked)
    pub new_role: Role,
    /// The real JID of the affected user
    pub real_jid: BareJid,
    /// Reason for the kick
    pub reason: Option<String>,
    /// Actor who performed the kick
    pub actor: Option<BareJid>,
}

/// MUC status codes per XEP-0045
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucStatusCode {
    /// 110: Self-presence (included in presence about oneself)
    SelfPresence = 110,
    /// 301: User was banned from room (affiliation changed to outcast)
    Banned = 301,
    /// 307: User was kicked from room (role changed to none)
    Kicked = 307,
    /// 321: User was removed due to affiliation change
    AffiliationChange = 321,
}

/// Information needed to build a kick/ban presence notification.
#[derive(Debug, Clone)]
pub struct KickBanInfo {
    /// The nick being kicked/banned
    pub nick: String,
    /// The real JID of the user being kicked/banned
    pub real_jid: BareJid,
    /// Their affiliation (Outcast for ban, unchanged for kick)
    pub affiliation: Affiliation,
    /// The reason for the action
    pub reason: Option<String>,
    /// The actor (JID) who performed the action
    pub actor: Option<BareJid>,
    /// Status code (301 for ban, 307 for kick)
    pub status_code: MucStatusCode,
}

/// Build a role query result (for GET requests querying users by role).
pub fn build_role_result(
    iq_id: &str,
    from_room_jid: &BareJid,
    to_jid: &Jid,
    items: &[(String, Role, Option<BareJid>)], // (nick, role, optional jid)
) -> Iq {
    let mut query = Element::builder("query", NS_MUC_ADMIN);

    for (nick, role, jid) in items {
        let mut item_builder = Element::builder("item", NS_MUC_ADMIN)
            .attr("nick", nick.as_str())
            .attr("role", role_to_str(*role));

        if let Some(j) = jid {
            item_builder = item_builder.attr("jid", j.to_string());
        }

        query = query.append(item_builder.build());
    }

    Iq {
        from: Some(Jid::from(from_room_jid.clone())),
        to: Some(to_jid.clone()),
        id: iq_id.to_string(),
        payload: IqType::Result(Some(query.build())),
    }
}

/// Check if an admin query is for role changes (kick operations).
///
/// Role change queries have items with 'nick' and 'role' attributes,
/// while affiliation change queries have items with 'jid' and 'affiliation'.
pub fn is_role_change_query(items: &[AdminItem]) -> bool {
    items.iter().any(|item| item.nick.is_some() && item.role.is_some())
}

/// Check if an admin query is for affiliation changes.
pub fn is_affiliation_change_query(items: &[AdminItem]) -> bool {
    items.iter().any(|item| item.affiliation.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_admin_get_iq(room_jid: &str, affiliation: &str) -> Iq {
        let query = Element::builder("query", NS_MUC_ADMIN)
            .append(
                Element::builder("item", NS_MUC_ADMIN)
                    .attr("affiliation", affiliation)
                    .build()
            )
            .build();

        Iq {
            from: Some("user@example.com/res".parse().unwrap()),
            to: Some(room_jid.parse().unwrap()),
            id: "test-1".to_string(),
            payload: IqType::Get(query),
        }
    }

    fn make_admin_set_iq(room_jid: &str, target_jid: &str, affiliation: &str) -> Iq {
        let query = Element::builder("query", NS_MUC_ADMIN)
            .append(
                Element::builder("item", NS_MUC_ADMIN)
                    .attr("jid", target_jid)
                    .attr("affiliation", affiliation)
                    .build()
            )
            .build();

        Iq {
            from: Some("owner@example.com/res".parse().unwrap()),
            to: Some(room_jid.parse().unwrap()),
            id: "test-2".to_string(),
            payload: IqType::Set(query),
        }
    }

    #[test]
    fn test_is_muc_admin_get() {
        let iq = make_admin_get_iq("room@muc.example.com", "member");
        assert!(is_muc_admin_get(&iq));
        assert!(!is_muc_admin_set(&iq));
    }

    #[test]
    fn test_is_muc_admin_set() {
        let iq = make_admin_set_iq("room@muc.example.com", "user@example.com", "admin");
        assert!(is_muc_admin_set(&iq));
        assert!(!is_muc_admin_get(&iq));
    }

    #[test]
    fn test_is_muc_admin_iq() {
        let iq = make_admin_get_iq("room@muc.example.com", "member");
        assert!(is_muc_admin_iq(&iq, "muc.example.com"));
        assert!(!is_muc_admin_iq(&iq, "other.domain.com"));
    }

    #[test]
    fn test_parse_admin_query_get() {
        let iq = make_admin_get_iq("room@muc.example.com", "member");
        let query = parse_admin_query(&iq, "muc.example.com").unwrap();

        assert_eq!(query.room_jid.to_string(), "room@muc.example.com");
        assert!(query.is_get);
        assert_eq!(query.items.len(), 1);
        assert_eq!(query.items[0].affiliation, Some(Affiliation::Member));
    }

    #[test]
    fn test_parse_admin_query_set() {
        let iq = make_admin_set_iq("room@muc.example.com", "user@example.com", "admin");
        let query = parse_admin_query(&iq, "muc.example.com").unwrap();

        assert_eq!(query.room_jid.to_string(), "room@muc.example.com");
        assert!(!query.is_get);
        assert_eq!(query.items.len(), 1);
        assert_eq!(query.items[0].jid.as_ref().unwrap().to_string(), "user@example.com");
        assert_eq!(query.items[0].affiliation, Some(Affiliation::Admin));
    }

    #[test]
    fn test_parse_affiliation() {
        assert_eq!(parse_muc_affiliation("owner").unwrap(), Affiliation::Owner);
        assert_eq!(parse_muc_affiliation("admin").unwrap(), Affiliation::Admin);
        assert_eq!(parse_muc_affiliation("member").unwrap(), Affiliation::Member);
        assert_eq!(parse_muc_affiliation("none").unwrap(), Affiliation::None);
        assert_eq!(parse_muc_affiliation("outcast").unwrap(), Affiliation::Outcast);
        assert!(parse_muc_affiliation("invalid").is_err());
    }

    #[test]
    fn test_parse_role() {
        assert_eq!(parse_muc_role("moderator").unwrap(), Role::Moderator);
        assert_eq!(parse_muc_role("participant").unwrap(), Role::Participant);
        assert_eq!(parse_muc_role("visitor").unwrap(), Role::Visitor);
        assert_eq!(parse_muc_role("none").unwrap(), Role::None);
        assert!(parse_muc_role("invalid").is_err());
    }

    #[test]
    fn test_build_admin_result() {
        let items = vec![
            ("user1@example.com".parse().unwrap(), Affiliation::Member),
            ("user2@example.com".parse().unwrap(), Affiliation::Admin),
        ];

        let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let to_jid: Jid = "requester@example.com".parse().unwrap();

        let result = build_admin_result("test-1", &room_jid, &to_jid, &items);

        assert_eq!(result.id, "test-1");
        assert!(matches!(result.payload, IqType::Result(Some(_))));
    }

    #[test]
    fn test_build_admin_set_result() {
        let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let to_jid: Jid = "owner@example.com".parse().unwrap();

        let result = build_admin_set_result("test-2", &room_jid, &to_jid);

        assert_eq!(result.id, "test-2");
        assert!(matches!(result.payload, IqType::Result(None)));
    }

    #[test]
    fn test_parse_admin_query_with_reason() {
        let query = Element::builder("query", NS_MUC_ADMIN)
            .append(
                Element::builder("item", NS_MUC_ADMIN)
                    .attr("jid", "banned@example.com")
                    .attr("affiliation", "outcast")
                    .append(
                        Element::builder("reason", NS_MUC_ADMIN)
                            .append("Spamming")
                            .build()
                    )
                    .build()
            )
            .build();

        let iq = Iq {
            from: Some("owner@example.com/res".parse().unwrap()),
            to: Some("room@muc.example.com".parse().unwrap()),
            id: "ban-1".to_string(),
            payload: IqType::Set(query),
        };

        let parsed = parse_admin_query(&iq, "muc.example.com").unwrap();

        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].affiliation, Some(Affiliation::Outcast));
        assert_eq!(parsed.items[0].reason.as_deref(), Some("Spamming"));
    }

    #[test]
    fn test_parse_kick_query_with_nick() {
        let query = Element::builder("query", NS_MUC_ADMIN)
            .append(
                Element::builder("item", NS_MUC_ADMIN)
                    .attr("nick", "troublemaker")
                    .attr("role", "none")
                    .append(
                        Element::builder("reason", NS_MUC_ADMIN)
                            .append("Kicked for bad behavior")
                            .build()
                    )
                    .build()
            )
            .build();

        let iq = Iq {
            from: Some("moderator@example.com/res".parse().unwrap()),
            to: Some("room@muc.example.com".parse().unwrap()),
            id: "kick-1".to_string(),
            payload: IqType::Set(query),
        };

        let parsed = parse_admin_query(&iq, "muc.example.com").unwrap();

        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].nick.as_deref(), Some("troublemaker"));
        assert_eq!(parsed.items[0].role, Some(Role::None));
        assert_eq!(parsed.items[0].reason.as_deref(), Some("Kicked for bad behavior"));
    }
}
