//! Typed application-binding DTOs matching the independent native Flutter facade.

use jid::{BareJid, FullJid};
use serde::{Deserialize, Serialize};
use waddle_xmpp_client::messaging::MucAffiliation;
use waddle_xmpp_client::stickers::{
    compute_pack_id, HashAlgo, PackSticker, StickerDimensions, StickerHash, StickerPackDoc,
    StickerText,
};
use waddle_xmpp_client::xep::xep0045_owner::{RoomConfigPatch, RoomPinPermission};

#[derive(Debug, Deserialize)]
#[serde(tag = "method", content = "args", rename_all = "snake_case")]
pub(crate) enum ParityCommand {
    CreateRoom {
        localpart: String,
        nick: String,
        patch: ConfigPatch,
    },
    FetchRoomConfig {
        room_jid: BareJid,
    },
    SubmitRoomConfig {
        room_jid: BareJid,
        patch: ConfigPatch,
    },
    DestroyRoom {
        room_jid: BareJid,
        reason: Option<String>,
    },
    KickOccupant {
        room_jid: BareJid,
        nick: String,
        reason: Option<String>,
    },
    SetRoomAffiliation {
        room_jid: BareJid,
        target_jid: BareJid,
        #[serde(deserialize_with = "deserialize_affiliation")]
        affiliation: MucAffiliation,
        reason: Option<String>,
    },
    PublishAvatar {
        data: Vec<u8>,
        mime_type: String,
        width: u32,
        height: u32,
    },
    DisableAvatar {},
    FetchStickerPacks {
        owner_jid: Option<BareJid>,
    },
    FetchStickerPack {
        owner_jid: BareJid,
        node: Option<String>,
        pack_id: String,
    },
    PublishStickerPack {
        pack: StickerPack,
    },
    RetractStickerPack {
        pack_id: String,
    },
    LookupLinkPreview {
        url: String,
        scope_jid: BareJid,
    },
    #[serde(alias = "ping")]
    PingServer {},
    SendCallFinishWithReason {
        peer_full_jid: FullJid,
        sid: String,
        reason: Reason,
    },
}

fn deserialize_affiliation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<MucAffiliation, D::Error> {
    let value = String::deserialize(deserializer)?;
    MucAffiliation::from_attr(&value)
        .ok_or_else(|| serde::de::Error::custom("invalid room affiliation"))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PinPermission {
    AdminsOnly,
    Anyone,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConfigPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub forum: Option<bool>,
    pub pin_permission: Option<PinPermission>,
}

impl From<ConfigPatch> for RoomConfigPatch {
    fn from(value: ConfigPatch) -> Self {
        Self {
            name: value.name,
            description: value.description,
            forum: value.forum,
            pin_permission: value.pin_permission.map(|permission| match permission {
                PinPermission::AdminsOnly => RoomPinPermission::AdminsOnly,
                PinPermission::Anyone => RoomPinPermission::Anyone,
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Config {
    pub name: Option<String>,
    pub description: Option<String>,
    pub members_only: Option<bool>,
    pub public_room: Option<bool>,
    pub moderated: Option<bool>,
    pub forum: Option<bool>,
    pub pin_permission: Option<PinPermission>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FileHash {
    pub algo: String,
    pub value_b64: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Sticker {
    pub desc: String,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub hashes: Vec<FileHash>,
    pub sources: Vec<String>,
    pub suggests: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StickerPack {
    pub id: String,
    pub name: Option<String>,
    pub summary: Option<String>,
    pub restricted: bool,
    pub stickers: Vec<Sticker>,
}

pub(crate) fn pack_from_boundary(pack: StickerPack) -> Result<StickerPackDoc, &'static str> {
    let stickers = pack
        .stickers
        .into_iter()
        .map(|sticker| {
            if sticker.desc.trim().is_empty() || sticker.hashes.is_empty() {
                return Err("a sticker requires a textual fallback and a content hash");
            }
            let hashes = sticker
                .hashes
                .into_iter()
                .map(|hash| {
                    Ok(StickerHash {
                        algo: HashAlgo::from_attr(&hash.algo)
                            .ok_or("unsupported sticker hash algorithm")?,
                        value_b64: hash.value_b64,
                    })
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            let sources: Vec<String> = sticker
                .sources
                .into_iter()
                .filter(|url| !url.trim().is_empty())
                .collect();
            if sources.is_empty() {
                return Err("a sticker requires a source URL");
            }
            Ok(PackSticker {
                desc: sticker.desc,
                media_type: sticker.media_type,
                size: sticker.size,
                dimensions: match (sticker.width, sticker.height) {
                    (Some(width), Some(height)) => Some(StickerDimensions { width, height }),
                    _ => None,
                },
                hashes,
                sources,
                suggests: sticker
                    .suggests
                    .into_iter()
                    .map(StickerText::plain)
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    if stickers.is_empty() {
        return Err("a sticker pack must contain a sticker");
    }
    let name = pack
        .name
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .map(StickerText::plain)
        .collect::<Vec<_>>();
    let summary = pack
        .summary
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .map(StickerText::plain)
        .collect::<Vec<_>>();
    Ok(StickerPackDoc {
        id: compute_pack_id(&name, &summary, &stickers),
        name,
        summary,
        restricted: pack.restricted,
        stickers,
        pack_hash: None,
    })
}

pub(crate) fn pack_to_boundary(pack: StickerPackDoc) -> StickerPack {
    StickerPack {
        id: pack.id.to_string(),
        name: pack.name.into_iter().next().map(|text| text.text),
        summary: pack.summary.into_iter().next().map(|text| text.text),
        restricted: pack.restricted,
        stickers: pack
            .stickers
            .into_iter()
            .map(|sticker| Sticker {
                desc: sticker.desc,
                media_type: sticker.media_type,
                size: sticker.size,
                width: sticker.dimensions.map(|size| size.width),
                height: sticker.dimensions.map(|size| size.height),
                hashes: sticker
                    .hashes
                    .into_iter()
                    .map(|hash| FileHash {
                        algo: hash.algo.as_str().to_string(),
                        value_b64: hash.value_b64,
                    })
                    .collect(),
                sources: sticker.sources,
                suggests: sticker.suggests.into_iter().map(|text| text.text).collect(),
            })
            .collect(),
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    AlternativeSession,
    Busy,
    Cancel,
    ConnectivityError,
    Decline,
    Expired,
    FailedApplication,
    FailedTransport,
    GeneralError,
    Gone,
    IncompatibleParameters,
    MediaError,
    SecurityError,
    Success,
    Timeout,
    UnsupportedApplications,
    UnsupportedTransports,
}

impl Reason {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::AlternativeSession => "alternative-session",
            Self::Busy => "busy",
            Self::Cancel => "cancel",
            Self::ConnectivityError => "connectivity-error",
            Self::Decline => "decline",
            Self::Expired => "expired",
            Self::FailedApplication => "failed-application",
            Self::FailedTransport => "failed-transport",
            Self::GeneralError => "general-error",
            Self::Gone => "gone",
            Self::IncompatibleParameters => "incompatible-parameters",
            Self::MediaError => "media-error",
            Self::SecurityError => "security-error",
            Self::Success => "success",
            Self::Timeout => "timeout",
            Self::UnsupportedApplications => "unsupported-applications",
            Self::UnsupportedTransports => "unsupported-transports",
        }
    }
}
