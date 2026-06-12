//! Waddle group-DM protocol extensions.
//!
//! Group DMs are XEP-0045 rooms classified by `urn:waddle:group-dm:0`.
//! This module owns the Waddle-specific child that rides inside a
//! XEP-0045 mediated `<invite/>` to describe how much history a newly
//! added member may see.

use minidom::Element;

/// Group-DM room disco feature and invite-extension namespace.
pub const NS_GROUP_DM: &str = "urn:waddle:group-dm:0";

/// History visibility requested by the inviter for a newly added member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupDmHistoryAccess {
    /// New member may only query archive rows from their add/join boundary.
    FromJoin,
    /// New member may query the full group-DM archive.
    Full,
}

impl GroupDmHistoryAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FromJoin => "from-join",
            Self::Full => "full",
        }
    }
}

/// Build `<history-access xmlns='urn:waddle:group-dm:0' mode='…'/>`.
pub fn build_history_access(access: GroupDmHistoryAccess) -> Element {
    Element::builder("history-access", NS_GROUP_DM)
        .attr(
            minidom::rxml::xml_ncname!("mode").to_owned(),
            access.as_str(),
        )
        .build()
}

/// Parse a Waddle group-DM history-access child.
///
/// Missing or unknown modes are treated as `from-join`, the privacy-preserving
/// default required by the group-DM add-members flow.
pub fn parse_history_access(element: &Element) -> Option<GroupDmHistoryAccess> {
    if !element.is("history-access", NS_GROUP_DM) {
        return None;
    }

    Some(match element.attr("mode") {
        Some("full") => GroupDmHistoryAccess::Full,
        _ => GroupDmHistoryAccess::FromJoin,
    })
}

/// Return the first history-access child under a XEP-0045 mediated invite.
pub fn history_access_from_mediated_invite(invite: &Element) -> Option<GroupDmHistoryAccess> {
    invite
        .children()
        .find_map(|child| parse_history_access(child))
}
