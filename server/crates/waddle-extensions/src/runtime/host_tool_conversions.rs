use chrono::{DateTime, Utc};
use xmpp_parsers::jid::BareJid;

use super::waddle::extension::types as wit_types;
use crate::host_tools as host_domain;
use crate::host_tools::{HostToolError, HostToolErrorCode};
use crate::types::{DisplayText, PubSubItemId, PubSubNode, RoomJid, ThreadId, Timestamp};

impl TryFrom<wit_types::ListChannelsRequest> for host_domain::ListChannelsRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::ListChannelsRequest) -> Result<Self, Self::Error> {
        let _ = value;
        Ok(Self)
    }
}

impl From<host_domain::ListChannelsResponse> for wit_types::ListChannelsResponse {
    fn from(value: host_domain::ListChannelsResponse) -> Self {
        Self {
            channels: value.channels.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::ChannelSummary> for wit_types::ChannelSummary {
    fn from(value: host_domain::ChannelSummary) -> Self {
        Self {
            room: RoomJid::new(value.room.to_string())
                .expect("host returned valid bare room jid")
                .into(),
            name: value.name.map(Into::into),
            description: value.description.map(Into::into),
        }
    }
}

impl TryFrom<wit_types::ListSpacesRequest> for host_domain::ListSpacesRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::ListSpacesRequest) -> Result<Self, Self::Error> {
        let _ = value;
        Ok(Self)
    }
}

impl From<host_domain::ListSpacesResponse> for wit_types::ListSpacesResponse {
    fn from(value: host_domain::ListSpacesResponse) -> Self {
        Self {
            spaces: value.spaces.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::SpaceSummary> for wit_types::SpaceSummary {
    fn from(value: host_domain::SpaceSummary) -> Self {
        Self {
            service: wit_types::BareJid {
                value: value.service.to_string(),
            },
            node: value.node.into(),
            name: value.name.map(Into::into),
            description: value.description.map(Into::into),
            channels: value.channels.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<wit_types::ListRoomMembersRequest> for host_domain::ListRoomMembersRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::ListRoomMembersRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            room: parse_bare_jid(value.room.value)?,
        })
    }
}

impl From<host_domain::ListRoomMembersResponse> for wit_types::ListRoomMembersResponse {
    fn from(value: host_domain::ListRoomMembersResponse) -> Self {
        Self {
            members: value.members.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::RoomMember> for wit_types::RoomMember {
    fn from(value: host_domain::RoomMember) -> Self {
        Self {
            room: RoomJid::new(value.room.to_string())
                .expect("host returned valid bare room jid")
                .into(),
            jid: wit_types::Jid {
                value: value.jid.to_string(),
            },
            nick: value.nick.map(Into::into),
            role: value.role.into(),
            affiliation: value.affiliation.into(),
        }
    }
}

impl From<host_domain::MucRole> for wit_types::MucRole {
    fn from(value: host_domain::MucRole) -> Self {
        match value {
            host_domain::MucRole::None => Self::None,
            host_domain::MucRole::Visitor => Self::Visitor,
            host_domain::MucRole::Participant => Self::Participant,
            host_domain::MucRole::Moderator => Self::Moderator,
        }
    }
}

impl From<host_domain::MucAffiliation> for wit_types::MucAffiliation {
    fn from(value: host_domain::MucAffiliation) -> Self {
        match value {
            host_domain::MucAffiliation::None => Self::None,
            host_domain::MucAffiliation::Outcast => Self::Outcast,
            host_domain::MucAffiliation::Member => Self::Member,
            host_domain::MucAffiliation::Admin => Self::Admin,
            host_domain::MucAffiliation::Owner => Self::Owner,
        }
    }
}

impl TryFrom<wit_types::GetPresenceRequest> for host_domain::GetPresenceRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::GetPresenceRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            subject: parse_bare_jid(value.subject.value)?,
        })
    }
}

impl From<host_domain::GetPresenceResponse> for wit_types::GetPresenceResponse {
    fn from(value: host_domain::GetPresenceResponse) -> Self {
        Self {
            resources: value.resources.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::PresenceState> for wit_types::PresenceState {
    fn from(value: host_domain::PresenceState) -> Self {
        Self {
            jid: wit_types::Jid {
                value: value.jid.to_string(),
            },
            availability: value.availability.into(),
            show: value.show.map(Into::into),
            status: value.status.map(Into::into),
            priority: value.priority,
        }
    }
}

impl From<host_domain::PresenceAvailability> for wit_types::PresenceAvailability {
    fn from(value: host_domain::PresenceAvailability) -> Self {
        match value {
            host_domain::PresenceAvailability::Available => Self::Available,
            host_domain::PresenceAvailability::Unavailable => Self::Unavailable,
        }
    }
}

impl From<host_domain::PresenceShow> for wit_types::PresenceShow {
    fn from(value: host_domain::PresenceShow) -> Self {
        match value {
            host_domain::PresenceShow::Chat => Self::Chat,
            host_domain::PresenceShow::Away => Self::Away,
            host_domain::PresenceShow::ExtendedAway => Self::Xa,
            host_domain::PresenceShow::DoNotDisturb => Self::Dnd,
        }
    }
}

impl TryFrom<wit_types::GetRosterRequest> for host_domain::GetRosterRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::GetRosterRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            owner: parse_bare_jid(value.owner.value)?,
        })
    }
}

impl From<host_domain::GetRosterResponse> for wit_types::GetRosterResponse {
    fn from(value: host_domain::GetRosterResponse) -> Self {
        Self {
            entries: value.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::RosterEntry> for wit_types::RosterEntry {
    fn from(value: host_domain::RosterEntry) -> Self {
        Self {
            jid: wit_types::BareJid {
                value: value.jid.to_string(),
            },
            name: value.name.map(Into::into),
            subscription: value.subscription.into(),
            ask: value.ask.map(Into::into),
            groups: value.groups.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::RosterSubscription> for wit_types::RosterSubscription {
    fn from(value: host_domain::RosterSubscription) -> Self {
        match value {
            host_domain::RosterSubscription::None => Self::None,
            host_domain::RosterSubscription::To => Self::SubscribedTo,
            host_domain::RosterSubscription::From => Self::SubscribedFrom,
            host_domain::RosterSubscription::Both => Self::Both,
            host_domain::RosterSubscription::Remove => Self::Remove,
        }
    }
}

impl From<host_domain::RosterAsk> for wit_types::RosterAsk {
    fn from(value: host_domain::RosterAsk) -> Self {
        match value {
            host_domain::RosterAsk::Subscribe => Self::Subscribe,
        }
    }
}

impl TryFrom<wit_types::MamQuery> for host_domain::MamQuery {
    type Error = HostToolError;

    fn try_from(value: wit_types::MamQuery) -> Result<Self, Self::Error> {
        Ok(Self {
            target: value.target.try_into()?,
            start: value
                .start
                .map(|timestamp| parse_timestamp(timestamp.value))
                .transpose()?,
            end: value
                .end
                .map(|timestamp| parse_timestamp(timestamp.value))
                .transpose()?,
            thread_id: value
                .thread_id
                .map(|thread_id| ThreadId::new(thread_id.value))
                .transpose()
                .map_err(host_type_error)?,
            sender: value
                .sender
                .map(|sender| parse_bare_jid(sender.value))
                .transpose()?,
            text: value
                .text
                .map(|text| {
                    DisplayText::new(text.value).map_err(|error| {
                        HostToolError::invalid_request(
                            DisplayText::new(error.to_string())
                                .expect("type error message is non-empty"),
                        )
                    })
                })
                .transpose()?,
            max_results: value.max_results,
        })
    }
}

impl TryFrom<wit_types::MamTarget> for host_domain::MamTarget {
    type Error = HostToolError;

    fn try_from(value: wit_types::MamTarget) -> Result<Self, Self::Error> {
        Ok(match value {
            wit_types::MamTarget::Room(room) => Self::Room(parse_bare_jid(room.value)?),
            wit_types::MamTarget::Conversation(jid) => {
                Self::Conversation(parse_bare_jid(jid.value)?)
            }
        })
    }
}

impl From<host_domain::MamQueryResponse> for wit_types::MamQueryResponse {
    fn from(value: host_domain::MamQueryResponse) -> Self {
        Self {
            messages: value.messages.into_iter().map(Into::into).collect(),
            complete: value.complete,
        }
    }
}

impl From<host_domain::ArchivedMessage> for wit_types::ArchivedMessage {
    fn from(value: host_domain::ArchivedMessage) -> Self {
        Self {
            stanza_id: value.stanza_id.into(),
            from_jid: wit_types::Jid {
                value: value.from.to_string(),
            },
            to_jid: wit_types::Jid {
                value: value.to.to_string(),
            },
            sent_at: Timestamp::new(value.sent_at.to_rfc3339())
                .expect("rfc3339 timestamp is non-empty")
                .into(),
            body: value.body.map(Into::into),
            thread_id: value.thread_id.map(Into::into),
            reply_to: value.reply_to.map(Into::into),
        }
    }
}

impl TryFrom<wit_types::SendMessageRequest> for host_domain::SendMessageRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::SendMessageRequest) -> Result<Self, Self::Error> {
        let body = DisplayText::new(value.body.value).map_err(|error| {
            HostToolError::invalid_request(
                DisplayText::new(error.to_string()).expect("type error message is non-empty"),
            )
        })?;
        let markup = value
            .markup
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;
        validate_message_markup(&body, &markup)?;
        Ok(Self {
            target: value.target.try_into()?,
            body,
            thread_id: value
                .thread_id
                .map(|thread_id| ThreadId::new(thread_id.value))
                .transpose()
                .map_err(host_type_error)?,
            reply_to: value.reply_to.map(TryInto::try_into).transpose().map_err(
                |error: anyhow::Error| {
                    HostToolError::invalid_request(
                        DisplayText::new(error.to_string())
                            .expect("type error message is non-empty"),
                    )
                },
            )?,
            markup,
            extensions: value
                .extensions
                .map(TryInto::try_into)
                .transpose()
                .map_err(|error: anyhow::Error| {
                    HostToolError::invalid_request(
                        DisplayText::new(error.to_string())
                            .expect("type error message is non-empty"),
                    )
                })?,
        })
    }
}

impl TryFrom<wit_types::MessageMarkupSpan> for host_domain::MessageMarkupSpan {
    type Error = HostToolError;

    fn try_from(value: wit_types::MessageMarkupSpan) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: value.kind.into(),
            start: value.start,
            end: value.end,
        })
    }
}

impl From<wit_types::MessageMarkupKind> for host_domain::MessageMarkupKind {
    fn from(value: wit_types::MessageMarkupKind) -> Self {
        match value {
            wit_types::MessageMarkupKind::Blockquote => Self::Blockquote,
        }
    }
}

fn validate_message_markup(
    body: &DisplayText,
    spans: &[host_domain::MessageMarkupSpan],
) -> Result<(), HostToolError> {
    let body_len = body.as_str().chars().count() as u32;
    for span in spans {
        if span.start >= span.end || span.end > body_len {
            return Err(HostToolError::invalid_request(
                DisplayText::new("message markup span range is outside the body")
                    .expect("static message is non-empty"),
            ));
        }
    }
    for (index, left) in spans.iter().enumerate() {
        for right in &spans[index + 1..] {
            let overlaps = left.start < right.end && right.start < left.end;
            if overlaps {
                return Err(HostToolError::invalid_request(
                    DisplayText::new("message markup block ranges must not overlap")
                        .expect("static message is non-empty"),
                ));
            }
        }
    }
    Ok(())
}

impl TryFrom<wit_types::MessageTarget> for host_domain::MessageTarget {
    type Error = HostToolError;

    fn try_from(value: wit_types::MessageTarget) -> Result<Self, Self::Error> {
        Ok(match value {
            wit_types::MessageTarget::Muc(room) => Self::Muc(parse_bare_jid(room.value)?),
            wit_types::MessageTarget::Direct(jid) => Self::Direct(parse_bare_jid(jid.value)?),
        })
    }
}

impl From<host_domain::SendMessageResponse> for wit_types::SendMessageResponse {
    fn from(value: host_domain::SendMessageResponse) -> Self {
        Self {
            stanza_id: value.stanza_id.into(),
        }
    }
}

impl TryFrom<wit_types::PubsubGetItemsRequest> for host_domain::PubSubGetItemsRequest {
    type Error = HostToolError;

    fn try_from(value: wit_types::PubsubGetItemsRequest) -> Result<Self, Self::Error> {
        let node = PubSubNode::new(value.node.value).map_err(host_type_error)?;
        let item_ids = value
            .item_ids
            .into_iter()
            .map(|id| PubSubItemId::new(id.value).map_err(host_type_error))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            node,
            max_items: value.max_items,
            item_ids,
        })
    }
}

impl From<host_domain::PubSubGetItemsResponse> for wit_types::PubsubGetItemsResponse {
    fn from(value: host_domain::PubSubGetItemsResponse) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host_domain::PubSubStoredItem> for wit_types::PubsubStoredItem {
    fn from(value: host_domain::PubSubStoredItem) -> Self {
        Self {
            id: value.id.into(),
            payload: value.payload.into(),
            publisher: value.publisher.map(|jid| wit_types::BareJid {
                value: jid.to_string(),
            }),
        }
    }
}

impl From<HostToolError> for wit_types::HostToolError {
    fn from(value: HostToolError) -> Self {
        Self {
            code: value.code.into(),
            message: value.message.into(),
        }
    }
}

impl From<HostToolErrorCode> for wit_types::HostToolErrorCode {
    fn from(value: HostToolErrorCode) -> Self {
        match value {
            HostToolErrorCode::Denied => Self::Denied,
            HostToolErrorCode::InvalidRequest => Self::InvalidRequest,
            HostToolErrorCode::NotFound => Self::NotFound,
            HostToolErrorCode::Unsupported => Self::Unsupported,
            HostToolErrorCode::TemporaryFailure => Self::TemporaryFailure,
        }
    }
}

fn parse_bare_jid(value: String) -> std::result::Result<BareJid, HostToolError> {
    value.parse::<BareJid>().map_err(|_| {
        HostToolError::invalid_request(
            DisplayText::new("invalid bare JID").expect("static host-tool error is non-empty"),
        )
    })
}

fn parse_timestamp(value: String) -> std::result::Result<DateTime<Utc>, HostToolError> {
    DateTime::parse_from_rfc3339(&value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| {
            HostToolError::invalid_request(
                DisplayText::new("invalid RFC3339 timestamp")
                    .expect("static host-tool error is non-empty"),
            )
        })
}

fn host_type_error(error: crate::types::FrameworkTypeError) -> HostToolError {
    HostToolError::invalid_request(
        DisplayText::new(error.to_string()).expect("type error message is non-empty"),
    )
}
