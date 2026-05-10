//! Read-modify-write helpers for the two vCard surfaces:
//! - XEP-0054 vcard-temp (`<vCard xmlns="vcard-temp"/>`)
//! - XEP-0292 vCard4 PEP item (`<vcard xmlns="urn:ietf:params:xml:ns:vcard-4.0"/>`)
//!
//! Each helper takes the existing element (from storage) plus the new
//! values and returns a freshly built element with PHOTO/FN replaced
//! or inserted, leaving every other field untouched.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use waddle_xmpp::xep::xep0054::NS_VCARD as NS_VCARD_TEMP;
use waddle_xmpp::xep::xep0292::NS_VCARD4;
use xmpp_parsers::minidom::Element;

/// PHOTO update payload — bytes and their MIME co-presented so the
/// type system enforces "if you have one you have the other" and the
/// builder can never silently default to `image/png` when upstream
/// forgot to pass a MIME.
#[derive(Debug, Clone, Copy)]
pub struct PhotoUpdate<'a> {
    pub bytes: &'a [u8],
    pub mime: &'a str,
}

/// Apply the requested PHOTO/FN updates to an XEP-0054 vcard-temp
/// element. `existing` is `None` for users who never had a vCard.
pub fn apply_vcard_temp_update(
    existing: Option<&Element>,
    photo: Option<PhotoUpdate<'_>>,
    fn_text: Option<&str>,
) -> Element {
    let mut builder = Element::builder("vCard", NS_VCARD_TEMP);

    let mut handled_photo = false;
    let mut handled_fn = false;

    if let Some(existing) = existing {
        for child in existing.children() {
            match child.name() {
                "PHOTO" if photo.is_some() => {
                    handled_photo = true;
                    let p = photo.expect("matched is_some");
                    builder = builder.append(build_vcard_temp_photo(p.bytes, p.mime));
                }
                "FN" if fn_text.is_some() => {
                    handled_fn = true;
                    builder =
                        builder.append(build_vcard_temp_fn(fn_text.expect("matched is_some")));
                }
                _ => {
                    builder = builder.append(child.clone());
                }
            }
        }
    }

    if !handled_photo {
        if let Some(p) = photo {
            builder = builder.append(build_vcard_temp_photo(p.bytes, p.mime));
        }
    }
    if !handled_fn {
        if let Some(name) = fn_text {
            builder = builder.append(build_vcard_temp_fn(name));
        }
    }

    builder.build()
}

/// vCard4 photo reference — a URI pointing at the canonical avatar
/// bytes (the `urn:xmpp:avatar:data` PEP item) plus the MIME of the
/// referenced bytes.
///
/// Why a URI rather than an inline `data:` payload: vCard4 items
/// fan out via XEP-0163 §3 to every roster contact's `+notify`
/// resource. Embedding base64 PNG bytes makes each fan-out stanza
/// the size of the avatar (~133 KB at the 100 KB cap), risking
/// per-stanza limits and bandwidth amplification. The published
/// `urn:xmpp:avatar:data` PEP node already holds the bytes; vCard4
/// just references them.
#[derive(Debug, Clone, Copy)]
pub struct Vcard4PhotoRef<'a> {
    pub uri: &'a str,
    pub mime: &'a str,
}

/// Apply the requested PHOTO/FN updates to a XEP-0292 vCard4 element.
/// `existing` is `None` for users without a published vCard4 item.
pub fn apply_vcard4_update(
    existing: Option<&Element>,
    photo: Option<Vcard4PhotoRef<'_>>,
    fn_text: Option<&str>,
) -> Element {
    let mut builder = Element::builder("vcard", NS_VCARD4);

    let mut handled_photo = false;
    let mut handled_fn = false;

    if let Some(existing) = existing {
        for child in existing.children() {
            match child.name() {
                "photo" if photo.is_some() => {
                    handled_photo = true;
                    let p = photo.expect("matched is_some");
                    builder = builder.append(build_vcard4_photo(p.uri, p.mime));
                }
                "fn" if fn_text.is_some() => {
                    handled_fn = true;
                    builder = builder.append(build_vcard4_fn(fn_text.expect("matched is_some")));
                }
                _ => {
                    builder = builder.append(child.clone());
                }
            }
        }
    }

    if !handled_photo {
        if let Some(p) = photo {
            builder = builder.append(build_vcard4_photo(p.uri, p.mime));
        }
    }
    if !handled_fn {
        if let Some(name) = fn_text {
            builder = builder.append(build_vcard4_fn(name));
        }
    }

    builder.build()
}

fn build_vcard_temp_photo(bytes: &[u8], mime: &str) -> Element {
    Element::builder("PHOTO", NS_VCARD_TEMP)
        .append(Element::builder("TYPE", NS_VCARD_TEMP).append(mime).build())
        .append(
            Element::builder("BINVAL", NS_VCARD_TEMP)
                .append(BASE64.encode(bytes).as_str())
                .build(),
        )
        .build()
}

fn build_vcard_temp_fn(name: &str) -> Element {
    Element::builder("FN", NS_VCARD_TEMP).append(name).build()
}

fn build_vcard4_photo(uri: &str, mime: &str) -> Element {
    // RFC 6350 §6.7.4: a vCard4 PHOTO is a single URI. Per RFC 6350
    // PARAMETERs land as XML attributes in the XEP-0292 mapping; we
    // surface the MIME via `mediatype` (RFC 6350 §5.10) so a client
    // that doesn't dereference the URI still knows the format.
    let mut photo = Element::builder("photo", NS_VCARD4);
    if !mime.is_empty() {
        photo = photo.append(
            Element::builder("parameters", NS_VCARD4)
                .append(
                    Element::builder("mediatype", NS_VCARD4)
                        .append(Element::builder("text", NS_VCARD4).append(mime).build())
                        .build(),
                )
                .build(),
        );
    }
    photo
        .append(Element::builder("uri", NS_VCARD4).append(uri).build())
        .build()
}

fn build_vcard4_fn(name: &str) -> Element {
    Element::builder("fn", NS_VCARD4)
        .append(Element::builder("text", NS_VCARD4).append(name).build())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(bytes: &'static [u8]) -> PhotoUpdate<'static> {
        PhotoUpdate {
            bytes,
            mime: "image/png",
        }
    }

    fn png_ref(uri: &'static str) -> Vcard4PhotoRef<'static> {
        Vcard4PhotoRef {
            uri,
            mime: "image/png",
        }
    }

    #[test]
    fn vcard_temp_inserts_photo_and_fn_when_absent() {
        let result = apply_vcard_temp_update(None, Some(png(b"\x89PNG")), Some("Alice"));
        let xml = String::from(&result);
        assert!(xml.contains("<PHOTO"), "{xml}");
        assert!(xml.contains("<TYPE"), "{xml}");
        assert!(xml.contains("<BINVAL"), "{xml}");
        assert!(xml.contains("<FN"), "{xml}");
        assert!(xml.contains("Alice"), "{xml}");
    }

    #[test]
    fn vcard_temp_replaces_existing_photo_and_fn_preserving_other_fields() {
        let existing: Element = "<vCard xmlns='vcard-temp'><FN>Old Name</FN><PHOTO><TYPE>image/jpeg</TYPE><BINVAL>OLD</BINVAL></PHOTO><EMAIL>kept@example.test</EMAIL><NOTE>Custom</NOTE></vCard>"
            .parse()
            .unwrap();
        let result =
            apply_vcard_temp_update(Some(&existing), Some(png(b"NEWBYTES")), Some("New Name"));
        let xml = String::from(&result);
        assert!(xml.contains("New Name"), "{xml}");
        assert!(!xml.contains("Old Name"), "{xml}");
        assert!(xml.contains("kept@example.test"), "EMAIL preserved: {xml}");
        assert!(xml.contains("Custom"), "NOTE preserved: {xml}");
        assert!(xml.contains("image/png"), "PHOTO TYPE updated: {xml}");
        assert!(!xml.contains("OLD"), "{xml}");
    }

    #[test]
    fn vcard_temp_fn_only_does_not_touch_photo() {
        let existing: Element = "<vCard xmlns='vcard-temp'><FN>Old</FN><PHOTO><TYPE>image/jpeg</TYPE><BINVAL>KEEP</BINVAL></PHOTO></vCard>"
            .parse()
            .unwrap();
        let result = apply_vcard_temp_update(Some(&existing), None, Some("New"));
        let xml = String::from(&result);
        assert!(xml.contains("New"), "{xml}");
        assert!(
            xml.contains("KEEP"),
            "PHOTO untouched when only FN is supplied: {xml}"
        );
    }

    #[test]
    fn vcard4_inserts_photo_uri_reference() {
        let result = apply_vcard4_update(
            None,
            Some(png_ref(
                "xmpp:alice@example.com?pubsub;node=urn:xmpp:avatar:data;item=abc",
            )),
            Some("Alice"),
        );
        let xml = String::from(&result);
        assert!(xml.contains("<photo"), "{xml}");
        assert!(xml.contains("<uri"), "{xml}");
        assert!(
            xml.contains("xmpp:alice@example.com?pubsub") && xml.contains("item=abc"),
            "vCard4 photo URI must reference the avatar-data PEP item: {xml}"
        );
        assert!(
            !xml.contains("data:image/png;base64,"),
            "vCard4 MUST NOT embed bytes inline (XEP-0163 fan-out bloat): {xml}"
        );
        assert!(xml.contains("<fn"), "{xml}");
        assert!(xml.contains("Alice"), "{xml}");
    }

    #[test]
    fn vcard4_replaces_photo_preserving_other_fields() {
        let existing: Element = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'><fn><text>Old</text></fn><photo><uri>xmpp:alice@example.com?pubsub;node=urn:xmpp:avatar:data;item=OLD</uri></photo><nickname><text>Pal</text></nickname></vcard>"
            .parse()
            .unwrap();
        let result = apply_vcard4_update(
            Some(&existing),
            Some(png_ref(
                "xmpp:alice@example.com?pubsub;node=urn:xmpp:avatar:data;item=NEW",
            )),
            Some("New"),
        );
        let xml = String::from(&result);
        assert!(xml.contains("New"), "{xml}");
        assert!(!xml.contains("<text>Old</text>"), "{xml}");
        assert!(xml.contains("Pal"), "nickname preserved: {xml}");
        assert!(xml.contains("item=NEW"), "{xml}");
        assert!(!xml.contains("item=OLD"), "{xml}");
    }
}
