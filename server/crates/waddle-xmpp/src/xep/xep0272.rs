//! XEP-0272: Multiparty Jingle (Muji).
//!
//! Muji coordinates a multi-party call inside a MUC. Each occupant
//! advertises participation by attaching a `<muji/>` element to their
//! MUC presence. Joining the SFU is a Jingle session-initiate that
//! embeds the same `<muji room='…'/>` element inside `<jingle/>`
//! (XEP-0272 §Joining).
//!
//! Wire shape — MUC presence (no `room` attribute, the room is the
//! `to`):
//!
//! ```xml
//! <presence to='room@muc/alice'>
//!   <muji xmlns='urn:xmpp:jingle:muji:0'>
//!     <preparing/>
//!     <content creator='initiator' name='audio'>
//!       <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
//!     </content>
//!   </muji>
//! </presence>
//! ```
//!
//! Wire shape — inside a Jingle session-initiate (the `room`
//! attribute identifies the MUC):
//!
//! ```xml
//! <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='…'>
//!   <muji xmlns='urn:xmpp:jingle:muji:0' room='room@muc'/>
//!   <content creator='initiator' name='audio'>…</content>
//! </jingle>
//! ```
//!
//! Two-phase joining (XEP-0272 §Joining): a client first advertises
//! `<preparing/>` in MUC presence and awaits the room's rebroadcast,
//! then re-emits presence with the `<content/>` children it offers.
//! Leaving is the absence of `<muji/>` from presence (XEP-0272
//! §Leaving).

use jid::BareJid;
use minidom::{rxml::xml_ncname, Element};
use thiserror::Error;

use crate::xep::xep0167::MediaKind;

pub use xmpp_parsers::jingle::ContentId;

/// Creator of a `<content/>` element. Mirrors XEP-0166 §7.3 (and
/// XEP-0272 reuses the same attribute on its `<content/>` children).
/// We keep a local copy rather than reusing
/// [`xmpp_parsers::jingle::Creator`] because the upstream type does
/// not implement `Eq`/`Copy`, which we need for room-state storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Creator {
    Initiator,
    Responder,
}

pub const NS_MUJI: &str = "urn:xmpp:jingle:muji:0";

const MUJI_NAME: &str = "muji";
const PREPARING_NAME: &str = "preparing";
const CONTENT_NAME: &str = "content";
const DESCRIPTION_NAME: &str = "description";

fn attr_room() -> minidom::rxml::NcName {
    xml_ncname!("room").to_owned()
}
fn attr_creator() -> minidom::rxml::NcName {
    xml_ncname!("creator").to_owned()
}
fn attr_name() -> minidom::rxml::NcName {
    xml_ncname!("name").to_owned()
}
fn attr_media() -> minidom::rxml::NcName {
    xml_ncname!("media").to_owned()
}

const ATTR_ROOM: &str = "room";
const ATTR_CREATOR: &str = "creator";
const ATTR_NAME: &str = "name";
const ATTR_MEDIA: &str = "media";

/// One `<content/>` advertisement inside a Muji `<muji/>` element.
///
/// The full XEP-0272 shape allows arbitrary RTP descriptions with
/// per-payload-type negotiation; Waddle relies on LiveKit to dictate
/// codecs, so the advertised description carries only a `media`
/// attribute. This stays XEP-0167-conformant — §3.2 only requires
/// `media`; `<payload-type/>` children are optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MujiContent {
    pub name: ContentId,
    pub creator: Creator,
    pub media: MediaKind,
}

impl MujiContent {
    pub fn new(name: impl Into<String>, creator: Creator, media: MediaKind) -> Self {
        Self {
            name: ContentId(name.into()),
            creator,
            media,
        }
    }

    fn to_element(&self) -> Element {
        let description = Element::builder(DESCRIPTION_NAME, crate::xep::xep0167::NS_JINGLE_RTP)
            .attr(attr_media(), self.media.as_str())
            .build();
        Element::builder(CONTENT_NAME, NS_MUJI)
            .attr(attr_creator(), creator_as_attr(self.creator))
            .attr(attr_name(), self.name.0.as_str())
            .append(description)
            .build()
    }

    fn try_from_element(elem: &Element) -> Result<Self, MujiParseError> {
        if elem.name() != CONTENT_NAME {
            return Err(MujiParseError::UnexpectedChild(elem.name().to_owned()));
        }
        let creator_raw = elem
            .attr(ATTR_CREATOR)
            .ok_or(MujiParseError::MissingAttribute(ATTR_CREATOR))?;
        let creator = parse_creator(creator_raw)
            .ok_or_else(|| MujiParseError::InvalidCreator(creator_raw.to_owned()))?;
        let name_raw = elem
            .attr(ATTR_NAME)
            .ok_or(MujiParseError::MissingAttribute(ATTR_NAME))?;
        let description = elem
            .children()
            .find(|c| c.name() == DESCRIPTION_NAME && c.ns() == crate::xep::xep0167::NS_JINGLE_RTP)
            .ok_or(MujiParseError::MissingDescription)?;
        let media_raw = description
            .attr(ATTR_MEDIA)
            .ok_or(MujiParseError::MissingAttribute(ATTR_MEDIA))?;
        let media = MediaKind::parse(media_raw)
            .ok_or_else(|| MujiParseError::InvalidMedia(media_raw.to_owned()))?;
        Ok(Self {
            name: ContentId(name_raw.to_owned()),
            creator,
            media,
        })
    }
}

/// The `<muji xmlns='urn:xmpp:jingle:muji:0' [room='…']/>` element.
///
/// - `room` is `Some` when the element appears inside a Jingle
///   session-initiate addressed to the SFU mixer (XEP-0272 §Joining
///   §"Jingle session-initiate referencing a Muji conference"); it
///   is `None` when the element appears inside MUC occupant presence
///   (the MUC is implicit from the stanza's `to`).
/// - `preparing` is `true` when a `<preparing/>` child is present
///   (XEP-0272 §Joining two-phase flow).
/// - `contents` enumerates `<content/>` children. Empty + `!preparing`
///   means the participant is exiting the call and the stored entry
///   should be cleared.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Muji {
    pub room: Option<BareJid>,
    pub preparing: bool,
    pub contents: Vec<MujiContent>,
}

impl Muji {
    pub fn preparing() -> Self {
        Self {
            preparing: true,
            ..Self::default()
        }
    }

    pub fn with_contents(contents: Vec<MujiContent>) -> Self {
        Self {
            contents,
            ..Self::default()
        }
    }

    pub fn for_room(room: BareJid) -> Self {
        Self {
            room: Some(room),
            ..Self::default()
        }
    }

    /// True when the participant should be considered actively in
    /// the call (has at least one content advertised). Preparing-only
    /// presences are not active yet — the indicator only fires once
    /// contents are present (XEP-0272 §Joining).
    pub fn is_active(&self) -> bool {
        !self.contents.is_empty()
    }

    /// True when the entry should be cleared from room storage —
    /// neither preparing nor advertising any content. Equivalent to
    /// "the participant has left" per XEP-0272 §Leaving.
    pub fn is_empty(&self) -> bool {
        !self.preparing && self.contents.is_empty()
    }

    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder(MUJI_NAME, NS_MUJI);
        if let Some(room) = &self.room {
            builder = builder.attr(attr_room(), room.to_string());
        }
        if self.preparing {
            builder = builder.append(Element::builder(PREPARING_NAME, NS_MUJI).build());
        }
        for content in &self.contents {
            builder = builder.append(content.to_element());
        }
        builder.build()
    }
}

impl TryFrom<&Element> for Muji {
    type Error = MujiParseError;

    fn try_from(elem: &Element) -> Result<Self, Self::Error> {
        if elem.name() != MUJI_NAME || elem.ns() != NS_MUJI {
            return Err(MujiParseError::WrongElement);
        }
        let room = match elem.attr(ATTR_ROOM) {
            Some(raw) => {
                Some(BareJid::new(raw).map_err(|_| MujiParseError::InvalidRoom(raw.to_owned()))?)
            }
            None => None,
        };
        let mut preparing = false;
        let mut contents = Vec::new();
        for child in elem.children() {
            if child.ns() != NS_MUJI {
                continue;
            }
            match child.name() {
                PREPARING_NAME => preparing = true,
                CONTENT_NAME => contents.push(MujiContent::try_from_element(child)?),
                _ => {
                    return Err(MujiParseError::UnexpectedChild(child.name().to_owned()));
                }
            }
        }
        Ok(Self {
            room,
            preparing,
            contents,
        })
    }
}

/// Find the first `<muji xmlns='urn:xmpp:jingle:muji:0'/>` child of
/// the given parent element (typically a `<presence/>` or
/// `<jingle/>`). Returns `None` if absent.
pub fn find_muji(parent: &Element) -> Option<&Element> {
    parent
        .children()
        .find(|c| c.name() == MUJI_NAME && c.ns() == NS_MUJI)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MujiParseError {
    #[error("element is not <muji xmlns='urn:xmpp:jingle:muji:0'/>")]
    WrongElement,
    #[error("invalid room JID: {0}")]
    InvalidRoom(String),
    #[error("invalid creator attribute: {0}")]
    InvalidCreator(String),
    #[error("invalid media attribute: {0}")]
    InvalidMedia(String),
    #[error("missing required attribute: {0}")]
    MissingAttribute(&'static str),
    #[error("missing <description/> child")]
    MissingDescription,
    #[error("unexpected child element in <muji/>: {0}")]
    UnexpectedChild(String),
}

fn creator_as_attr(creator: Creator) -> &'static str {
    match creator {
        Creator::Initiator => "initiator",
        Creator::Responder => "responder",
    }
}

fn parse_creator(raw: &str) -> Option<Creator> {
    match raw {
        "initiator" => Some(Creator::Initiator),
        "responder" => Some(Creator::Responder),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparing_only_round_trips() {
        let muji = Muji::preparing();
        let elem = muji.to_element();
        assert_eq!(elem.name(), MUJI_NAME);
        assert_eq!(elem.ns(), NS_MUJI);
        assert!(elem.children().any(|c| c.name() == PREPARING_NAME));

        let reparsed = Muji::try_from(&elem).expect("preparing reparses");
        assert!(reparsed.preparing);
        assert!(reparsed.contents.is_empty());
        assert!(reparsed.room.is_none());
        assert!(!reparsed.is_active());
    }

    #[test]
    fn multi_content_audio_video_round_trips() {
        let muji = Muji::with_contents(vec![
            MujiContent::new("audio", Creator::Initiator, MediaKind::Audio),
            MujiContent::new("video", Creator::Initiator, MediaKind::Video),
        ]);
        let elem = muji.to_element();
        let reparsed = Muji::try_from(&elem).expect("contents reparse");
        assert_eq!(reparsed.contents.len(), 2);
        assert_eq!(reparsed.contents[0].name.0, "audio");
        assert_eq!(reparsed.contents[0].media, MediaKind::Audio);
        assert_eq!(reparsed.contents[1].name.0, "video");
        assert_eq!(reparsed.contents[1].media, MediaKind::Video);
        assert!(reparsed.is_active());
    }

    #[test]
    fn room_attribute_round_trips() {
        let room: BareJid = "room@muc.example.com".parse().unwrap();
        let muji = Muji::for_room(room.clone());
        let elem = muji.to_element();
        assert_eq!(elem.attr(ATTR_ROOM), Some("room@muc.example.com"));

        let reparsed = Muji::try_from(&elem).expect("room reparses");
        assert_eq!(reparsed.room.as_ref(), Some(&room));
    }

    #[test]
    fn empty_muji_is_leave_marker() {
        let muji = Muji::default();
        assert!(muji.is_empty());
        assert!(!muji.is_active());
    }

    #[test]
    fn rejects_wrong_namespace() {
        let xml = "<muji xmlns='urn:waddle:not-muji'/>";
        let elem: Element = xml.parse().unwrap();
        let err = Muji::try_from(&elem).expect_err("namespace check");
        assert_eq!(err, MujiParseError::WrongElement);
    }

    #[test]
    fn rejects_invalid_creator() {
        let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'>\
                     <content creator='moderator' name='audio'>\
                       <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>\
                     </content>\
                   </muji>";
        let elem: Element = xml.parse().unwrap();
        let err = Muji::try_from(&elem).expect_err("creator validation");
        assert!(matches!(err, MujiParseError::InvalidCreator(_)));
    }

    #[test]
    fn ignores_non_muji_children() {
        let xml = "<muji xmlns='urn:xmpp:jingle:muji:0'>\
                     <c xmlns='http://jabber.org/protocol/caps' node='x' ver='y' hash='sha-1'/>\
                     <preparing/>\
                   </muji>";
        let elem: Element = xml.parse().unwrap();
        let muji = Muji::try_from(&elem).expect("foreign children ignored");
        assert!(muji.preparing);
    }

    #[test]
    fn find_muji_locates_child_in_presence() {
        let xml = "<presence xmlns='jabber:client'>\
                     <muji xmlns='urn:xmpp:jingle:muji:0'><preparing/></muji>\
                   </presence>";
        let elem: Element = xml.parse().unwrap();
        let found = find_muji(&elem).expect("muji child found");
        assert_eq!(found.name(), MUJI_NAME);
    }

    #[test]
    fn ns_matches_xep() {
        assert_eq!(NS_MUJI, "urn:xmpp:jingle:muji:0");
    }
}
