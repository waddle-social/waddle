//! XEP-0486: MUC Avatars
//!
//! A room's avatar is a vCard-temp (XEP-0054) `PHOTO` published to the
//! room's bare JID by a privileged entity. The room advertises the
//! XEP-0153 SHA-1 hash of each published photo in the
//! `muc#roominfo_avatarhash` field of its `muc#roominfo` disco#info
//! extension form, so a client can learn of (and cache) the avatar
//! before it joins.
//!
//! This module holds the pure protocol logic: validating a room vCard
//! and computing the advertised hashes. Storage, authorization and the
//! status-104 notification live in the server.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use minidom::Element;

use super::xep0054::NS_VCARD;
use super::xep0153::compute_photo_hash;

/// Field of the `muc#roominfo` form carrying the avatar hashes
/// (xep-0486.xml: registry submission).
pub const FIELD_ROOMINFO_AVATARHASH: &str = "muc#roominfo_avatarhash";

/// Label the specification gives the `muc#roominfo_avatarhash` field.
pub const LABEL_ROOMINFO_AVATARHASH: &str = "Avatar hash";

/// Why a submitted room vCard cannot be published as a room avatar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoomVCardError {
    /// A non-empty vCard carries no `PHOTO`, so it publishes no avatar.
    #[error("the room vCard carries no PHOTO")]
    NoPhoto,
    /// A `PHOTO` lacks a `TYPE` media type or a `BINVAL` payload. A
    /// hash cannot be computed for an external `EXTVAL` reference.
    #[error("a PHOTO needs both a TYPE and a BINVAL")]
    IncompletePhoto,
    /// A `BINVAL` is not valid base64.
    #[error("a PHOTO BINVAL is not valid base64")]
    InvalidBinval,
}

/// What a `vcard-temp` set on a room asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomAvatarUpdate {
    /// An empty vCard: "Setting an empty vCard unpublishes the avatar."
    Unpublish,
    /// Publish the vCard; its photos hash to these XEP-0153 values, in
    /// document order.
    Publish { hashes: Vec<String> },
}

/// Classify the vCard of a `vcard-temp` set addressed to a room.
///
/// # Errors
///
/// [`RoomVCardError`] when a non-empty vCard does not describe a
/// hashable avatar.
pub fn parse_room_vcard(vcard: &Element) -> Result<RoomAvatarUpdate, RoomVCardError> {
    if vcard.children().next().is_none() {
        return Ok(RoomAvatarUpdate::Unpublish);
    }
    let hashes = avatar_hashes(vcard)?;
    if hashes.is_empty() {
        return Err(RoomVCardError::NoPhoto);
    }
    Ok(RoomAvatarUpdate::Publish { hashes })
}

/// The XEP-0153 hash of every `PHOTO` of a room vCard, in document order
/// ("Multiple <PHOTO/> elements may be present in a vCard ... the
/// 'muc#roominfo_avatarhash' field must contain a hash of all of them").
///
/// # Errors
///
/// [`RoomVCardError`] for a photo that cannot be hashed.
pub fn avatar_hashes(vcard: &Element) -> Result<Vec<String>, RoomVCardError> {
    vcard
        .children()
        .filter(|child| child.name() == "PHOTO" && child.ns() == NS_VCARD)
        .map(|photo| {
            let has_type = photo
                .get_child("TYPE", NS_VCARD)
                .is_some_and(|media_type| !media_type.text().trim().is_empty());
            let binval = photo
                .get_child("BINVAL", NS_VCARD)
                .map(Element::text)
                .filter(|binval| !binval.trim().is_empty());
            let (true, Some(binval)) = (has_type, binval) else {
                return Err(RoomVCardError::IncompletePhoto);
            };
            // BINVAL is commonly line-wrapped base64.
            let compact: String = binval.chars().filter(|c| !c.is_whitespace()).collect();
            let bytes = STANDARD
                .decode(compact)
                .map_err(|_| RoomVCardError::InvalidBinval)?;
            Ok(compute_photo_hash(&bytes))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // xep-0486.xml's example SVG avatar and its published hash.
    const SVG_BINVAL: &str = "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIzMiIgaGVpZ2h0PSIzMiI+CiA8cmVjdCB4PSIwIiB5PSIwIiB3aWR0aD0iMzIiIGhlaWdodD0iMzIiIGZpbGw9InJlZCIvPgo8L3N2Zz4K";

    fn vcard(xml: &str) -> Element {
        xml.parse().expect("valid vCard XML")
    }

    #[test]
    fn empty_vcard_unpublishes() {
        assert_eq!(
            parse_room_vcard(&vcard("<vCard xmlns='vcard-temp'/>")),
            Ok(RoomAvatarUpdate::Unpublish)
        );
    }

    #[test]
    fn photo_publishes_with_its_sha1() {
        let xml = format!(
            "<vCard xmlns='vcard-temp'><PHOTO><TYPE>image/svg+xml</TYPE><BINVAL>{SVG_BINVAL}</BINVAL></PHOTO></vCard>"
        );
        let Ok(RoomAvatarUpdate::Publish { hashes }) = parse_room_vcard(&vcard(&xml)) else {
            panic!("expected a published avatar");
        };
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0].len(), 40);
        assert!(hashes[0].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn line_wrapped_binval_hashes_like_the_compact_form() {
        let wrapped = format!("{}\n  {}", &SVG_BINVAL[..20], &SVG_BINVAL[20..]);
        let one = format!(
            "<vCard xmlns='vcard-temp'><PHOTO><TYPE>image/png</TYPE><BINVAL>{SVG_BINVAL}</BINVAL></PHOTO></vCard>"
        );
        let two = format!(
            "<vCard xmlns='vcard-temp'><PHOTO><TYPE>image/png</TYPE><BINVAL>{wrapped}</BINVAL></PHOTO></vCard>"
        );
        assert_eq!(
            avatar_hashes(&vcard(&one)),
            avatar_hashes(&vcard(&two)),
            "whitespace must not change the hash"
        );
    }

    #[test]
    fn every_photo_is_hashed_in_document_order() {
        let xml = format!(
            "<vCard xmlns='vcard-temp'>\
             <PHOTO><TYPE>image/svg+xml</TYPE><BINVAL>{SVG_BINVAL}</BINVAL></PHOTO>\
             <PHOTO><TYPE>image/png</TYPE><BINVAL>iVBORw0KGgo=</BINVAL></PHOTO>\
             </vCard>"
        );
        let hashes = avatar_hashes(&vcard(&xml)).expect("hashable");
        assert_eq!(hashes.len(), 2);
        assert_ne!(hashes[0], hashes[1]);
    }

    #[test]
    fn nonempty_vcard_without_photo_is_rejected() {
        assert_eq!(
            parse_room_vcard(&vcard("<vCard xmlns='vcard-temp'><FN>Garden</FN></vCard>")),
            Err(RoomVCardError::NoPhoto)
        );
    }

    #[test]
    fn external_or_typeless_photo_is_rejected() {
        assert_eq!(
            avatar_hashes(&vcard(
                "<vCard xmlns='vcard-temp'><PHOTO><EXTVAL>https://example.org/a.png</EXTVAL></PHOTO></vCard>"
            )),
            Err(RoomVCardError::IncompletePhoto)
        );
        assert_eq!(
            avatar_hashes(&vcard(&format!(
                "<vCard xmlns='vcard-temp'><PHOTO><BINVAL>{SVG_BINVAL}</BINVAL></PHOTO></vCard>"
            ))),
            Err(RoomVCardError::IncompletePhoto)
        );
    }

    #[test]
    fn invalid_base64_is_rejected() {
        assert_eq!(
            avatar_hashes(&vcard(
                "<vCard xmlns='vcard-temp'><PHOTO><TYPE>image/png</TYPE><BINVAL>***</BINVAL></PHOTO></vCard>"
            )),
            Err(RoomVCardError::InvalidBinval)
        );
    }
}
