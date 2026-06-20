//! Shared low-coupling domain types and helpers.

/// Basic waddle information for auto-join enumeration.
#[derive(Debug, Clone)]
pub struct WaddleInfo {
    /// Waddle ID
    pub id: String,
    /// Waddle name
    pub name: String,
}

/// Basic channel information for auto-join enumeration.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    /// Channel ID
    pub id: String,
    /// Channel name
    pub name: String,
    /// Channel type (e.g., "text", "forum")
    pub channel_type: String,
}

/// Channel-backed MUC room metadata.
#[derive(Debug, Clone)]
pub struct ChannelRoomInfo {
    /// Waddle ID that owns the channel.
    pub waddle_id: String,
    /// Channel metadata.
    pub channel: ChannelInfo,
}

/// Supported managed channel types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelType {
    /// Standard text chat channel.
    Text,
    /// Owner/moderator announcement channel.
    Announcement,
    /// Waddle thread-oriented channel.
    Forum,
    /// Private group direct-message channel.
    GroupDm,
}

impl ChannelType {
    /// Parse a stored channel type into a supported managed channel type.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "text" => Some(Self::Text),
            "announcement" => Some(Self::Announcement),
            "forum" => Some(Self::Forum),
            "group-dm" => Some(Self::GroupDm),
            _ => None,
        }
    }

    /// Convert this channel type to the canonical stored string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Announcement => "announcement",
            Self::Forum => "forum",
            Self::GroupDm => "group-dm",
        }
    }

    /// Returns true when this channel should be exposed as a forum room.
    pub fn is_forum(self) -> bool {
        matches!(self, Self::Forum)
    }
}

/// Build the canonical localpart for a managed channel room.
pub fn managed_room_localpart(channel_id: &str) -> String {
    channel_id.to_string()
}

/// Parse the canonical localpart for a managed channel room.
pub fn parse_managed_room_localpart(localpart: &str) -> Option<String> {
    if localpart.is_empty() {
        return None;
    }
    Some(localpart.to_string())
}

/// Parse a bare room JID into managed channel coordinates.
pub fn parse_managed_room_jid(room_jid: &jid::BareJid) -> Option<String> {
    parse_managed_room_localpart(room_jid.node()?.as_str())
}

/// Build the canonical bare JID for a managed channel room.
pub fn managed_room_jid(channel_id: &str, muc_domain: &str) -> Result<jid::BareJid, jid::Error> {
    format!("{}@{}", managed_room_localpart(channel_id), muc_domain).parse()
}

/// Detailed waddle information for XEP-0503 spaces service.
#[derive(Debug, Clone)]
pub struct WaddleDetails {
    /// Waddle ID
    pub id: String,
    /// Waddle name
    pub name: String,
    /// Waddle description
    pub description: Option<String>,
    /// Owner user ID
    pub owner_id: String,
    /// Icon URL
    pub icon_url: Option<String>,
    /// Whether the waddle is public
    pub is_public: bool,
    /// When the waddle was created (ISO 8601)
    pub created_at: String,
}

/// Information about a created upload slot (XEP-0363).
#[derive(Debug, Clone)]
pub struct UploadSlotInfo {
    /// URL for uploading the file (HTTP PUT).
    pub put_url: String,
    /// URL for retrieving the file (HTTP GET).
    pub get_url: String,
    /// Optional headers to include with the PUT request.
    pub put_headers: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_room_helpers_round_trip() {
        let localpart = managed_room_localpart("channel-9");
        assert_eq!(localpart, "channel-9");
        assert_eq!(
            parse_managed_room_localpart(&localpart),
            Some("channel-9".to_string())
        );

        let room_jid = managed_room_jid("channel-9", "muc.example.com").expect("managed room jid");
        assert_eq!(room_jid.to_string(), "channel-9@muc.example.com");
        assert_eq!(
            parse_managed_room_jid(&room_jid),
            Some("channel-9".to_string())
        );
    }

    #[test]
    fn managed_room_parser_rejects_invalid_localparts() {
        assert_eq!(parse_managed_room_localpart(""), None);
    }

    #[test]
    fn supported_channel_types_are_explicit() {
        assert_eq!(ChannelType::parse("text"), Some(ChannelType::Text));
        assert_eq!(
            ChannelType::parse("announcement"),
            Some(ChannelType::Announcement)
        );
        assert_eq!(ChannelType::parse("forum"), Some(ChannelType::Forum));
        assert_eq!(ChannelType::parse("group-dm"), Some(ChannelType::GroupDm));
        assert_eq!(ChannelType::parse("voice"), None);
    }
}
