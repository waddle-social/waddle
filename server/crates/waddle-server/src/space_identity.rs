//! Typed Spaces PubSub node identity and XEP-0106 JID projection helpers.

use std::fmt;
use std::ops::Deref;

use jid::BareJid;
use waddle_xmpp::xep::xep0106::unescape_node;

/// Exact XEP-0060 node id under the Spaces PubSub service.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpaceNode(String);

impl SpaceNode {
    pub fn new(node: impl Into<String>) -> Self {
        Self(node.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<String> for SpaceNode {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SpaceNode {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl AsRef<str> for SpaceNode {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Deref for SpaceNode {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Display for SpaceNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for SpaceNode {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<SpaceNode> for &str {
    fn eq(&self, other: &SpaceNode) -> bool {
        *self == other.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceProjectionError {
    WrongDomain,
    MissingLocalpart,
    InvalidNode(SpaceNode),
    MismatchedProjection,
}

pub fn space_node_name(space_jid: &BareJid) -> Option<SpaceNode> {
    space_jid
        .node()
        .map(|node| SpaceNode::from(unescape_node(node.as_ref())))
}

pub fn projected_space_node_from_jid_text(space_jid_raw: &str) -> SpaceNode {
    space_jid_raw
        .parse::<BareJid>()
        .ok()
        .and_then(|space_jid| space_node_name(&space_jid))
        .filter(|space_node| !space_node.as_str().is_empty())
        .unwrap_or_else(|| SpaceNode::from(space_jid_raw))
}

pub fn space_jid_for_node(spaces_jid: &BareJid, node: &SpaceNode) -> Option<BareJid> {
    if !is_projectable_space_node(node) {
        return None;
    }
    format!(
        "{}@{}",
        escape_space_node_projection(node.as_str()),
        spaces_jid.domain()
    )
    .parse()
    .ok()
}

pub fn is_projectable_space_node(node: &SpaceNode) -> bool {
    let node = node.as_str();
    !node.starts_with(' ') && !node.ends_with(' ')
}

fn escape_space_node_projection(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for (byte_idx, ch) in input.char_indices() {
        match ch {
            ' ' => result.push_str("\\20"),
            '"' => result.push_str("\\22"),
            '&' => result.push_str("\\26"),
            '\'' => result.push_str("\\27"),
            '/' => result.push_str("\\2f"),
            ':' => result.push_str("\\3a"),
            '<' => result.push_str("\\3c"),
            '>' => result.push_str("\\3e"),
            '@' => result.push_str("\\40"),
            '\\' if starts_xep0106_escape_sequence(&input[byte_idx + 1..]) => {
                result.push_str("\\5c");
            }
            '\\' => result.push('\\'),
            _ => result.push(ch),
        }
    }
    result
}

fn starts_xep0106_escape_sequence(input: &str) -> bool {
    let next_two: String = input.chars().take(2).collect();
    matches!(
        next_two.as_str(),
        "20" | "22" | "26" | "27" | "2f" | "3a" | "3c" | "3e" | "40" | "5c"
    )
}

pub fn canonical_space_projection(
    spaces_jid: &BareJid,
    space_jid: &BareJid,
    space_node: Option<&SpaceNode>,
) -> Result<(SpaceNode, BareJid), SpaceProjectionError> {
    if space_jid.domain() != spaces_jid.domain() {
        return Err(SpaceProjectionError::WrongDomain);
    }
    let node = match space_node {
        Some(node) => node.clone(),
        None => space_node_name(space_jid).ok_or(SpaceProjectionError::MissingLocalpart)?,
    };
    let Some(canonical) = space_jid_for_node(spaces_jid, &node) else {
        return Err(SpaceProjectionError::InvalidNode(node));
    };
    if &canonical != space_jid {
        return Err(SpaceProjectionError::MismatchedProjection);
    }
    Ok((node, canonical))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_jid_projection_distinguishes_escape_looking_space_nodes() {
        let spaces_jid: BareJid = "spaces.localhost".parse().expect("spaces jid");
        let slash_node = SpaceNode::from("music/a");
        let literal_escape_node = SpaceNode::from("music\\2fa");

        let slash_projection =
            space_jid_for_node(&spaces_jid, &slash_node).expect("slash projection");
        let literal_projection =
            space_jid_for_node(&spaces_jid, &literal_escape_node).expect("literal projection");

        assert_eq!(slash_projection.to_string(), "music\\2fa@spaces.localhost");
        assert_eq!(
            literal_projection.to_string(),
            "music\\5c2fa@spaces.localhost"
        );
        assert_ne!(slash_projection, literal_projection);
        assert_eq!(space_node_name(&slash_projection), Some(slash_node));
        assert_eq!(
            space_node_name(&literal_projection),
            Some(literal_escape_node)
        );
    }

    #[test]
    fn space_jid_projection_preserves_non_escape_backslash_sequences() {
        let spaces_jid: BareJid = "spaces.localhost".parse().expect("spaces jid");
        let node = SpaceNode::from("foo\\bar");

        let projection = space_jid_for_node(&spaces_jid, &node).expect("projection");

        assert_eq!(projection.to_string(), "foo\\bar@spaces.localhost");
        assert_eq!(space_node_name(&projection), Some(node));
    }

    #[test]
    fn space_jid_projection_rejects_boundary_space_nodes() {
        let spaces_jid: BareJid = "spaces.localhost".parse().expect("spaces jid");

        for node in [" ops", "ops ", " "] {
            assert_eq!(
                space_jid_for_node(&spaces_jid, &SpaceNode::from(node)),
                None,
                "{node:?} must not project to a jid-single value"
            );
        }

        assert_eq!(
            space_jid_for_node(&spaces_jid, &SpaceNode::from("ops team"))
                .expect("interior space projection")
                .to_string(),
            "ops\\20team@spaces.localhost"
        );
    }
}
