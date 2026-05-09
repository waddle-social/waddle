//! Read-modify-write helpers for the two vCard surfaces:
//! - XEP-0054 vcard-temp (`<vCard xmlns="vcard-temp"/>`)
//! - XEP-0292 vCard4 PEP item (`<vcard xmlns="urn:ietf:params:xml:ns:vcard-4.0"/>`)
//!
//! Each helper takes the existing element (from storage) plus the new
//! values and returns a freshly built element with PHOTO/FN replaced
//! or inserted, leaving every other field untouched.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use xmpp_parsers::minidom::Element;

pub const NS_VCARD_TEMP: &str = "vcard-temp";
pub const NS_VCARD4: &str = "urn:ietf:params:xml:ns:vcard-4.0";

/// Apply the requested PHOTO/FN updates to an XEP-0054 vcard-temp
/// element. `existing` is `None` for users who never had a vCard.
pub fn apply_vcard_temp_update(
    existing: Option<&Element>,
    photo_bytes: Option<&[u8]>,
    photo_mime: Option<&str>,
    fn_text: Option<&str>,
) -> Element {
    let mut builder = Element::builder("vCard", NS_VCARD_TEMP);

    let mut handled_photo = false;
    let mut handled_fn = false;

    if let Some(existing) = existing {
        for child in existing.children() {
            match child.name() {
                "PHOTO" if photo_bytes.is_some() => {
                    handled_photo = true;
                    builder = builder.append(build_vcard_temp_photo(
                        photo_bytes.expect("matched is_some"),
                        photo_mime.unwrap_or("image/png"),
                    ));
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
        if let (Some(bytes), Some(mime)) = (photo_bytes, photo_mime) {
            builder = builder.append(build_vcard_temp_photo(bytes, mime));
        }
    }
    if !handled_fn {
        if let Some(name) = fn_text {
            builder = builder.append(build_vcard_temp_fn(name));
        }
    }

    builder.build()
}

/// Apply the requested PHOTO/FN updates to a XEP-0292 vCard4 element.
/// `existing` is `None` for users without a published vCard4 item.
pub fn apply_vcard4_update(
    existing: Option<&Element>,
    photo_bytes: Option<&[u8]>,
    photo_mime: Option<&str>,
    fn_text: Option<&str>,
) -> Element {
    let mut builder = Element::builder("vcard", NS_VCARD4);

    let mut handled_photo = false;
    let mut handled_fn = false;

    if let Some(existing) = existing {
        for child in existing.children() {
            match child.name() {
                "photo" if photo_bytes.is_some() => {
                    handled_photo = true;
                    builder = builder.append(build_vcard4_photo(
                        photo_bytes.expect("matched is_some"),
                        photo_mime.unwrap_or("image/png"),
                    ));
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
        if let (Some(bytes), Some(mime)) = (photo_bytes, photo_mime) {
            builder = builder.append(build_vcard4_photo(bytes, mime));
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

fn build_vcard4_photo(bytes: &[u8], mime: &str) -> Element {
    let data_uri = format!("data:{mime};base64,{}", BASE64.encode(bytes));
    Element::builder("photo", NS_VCARD4)
        .append(
            Element::builder("uri", NS_VCARD4)
                .append(data_uri.as_str())
                .build(),
        )
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

    #[test]
    fn vcard_temp_inserts_photo_and_fn_when_absent() {
        let result =
            apply_vcard_temp_update(None, Some(b"\x89PNG"), Some("image/png"), Some("Alice"));
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
        let result = apply_vcard_temp_update(
            Some(&existing),
            Some(b"NEWBYTES"),
            Some("image/png"),
            Some("New Name"),
        );
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
        let result = apply_vcard_temp_update(Some(&existing), None, None, Some("New"));
        let xml = String::from(&result);
        assert!(xml.contains("New"), "{xml}");
        assert!(
            xml.contains("KEEP"),
            "PHOTO untouched when only FN is supplied: {xml}"
        );
    }

    #[test]
    fn vcard4_inserts_photo_with_data_uri() {
        let result = apply_vcard4_update(None, Some(b"PNGDATA"), Some("image/png"), Some("Alice"));
        let xml = String::from(&result);
        assert!(xml.contains("<photo"), "{xml}");
        assert!(xml.contains("<uri"), "{xml}");
        assert!(xml.contains("data:image/png;base64,"), "{xml}");
        assert!(xml.contains("<fn"), "{xml}");
        assert!(xml.contains("Alice"), "{xml}");
    }

    #[test]
    fn vcard4_replaces_photo_preserving_other_fields() {
        let existing: Element = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'><fn><text>Old</text></fn><photo><uri>data:image/jpeg;base64,OLD</uri></photo><nickname><text>Pal</text></nickname></vcard>"
            .parse()
            .unwrap();
        let result = apply_vcard4_update(
            Some(&existing),
            Some(b"NEW"),
            Some("image/png"),
            Some("New"),
        );
        let xml = String::from(&result);
        assert!(xml.contains("New"), "{xml}");
        assert!(!xml.contains("Old"), "{xml}");
        assert!(xml.contains("Pal"), "nickname preserved: {xml}");
        assert!(xml.contains("data:image/png;base64,"), "{xml}");
        assert!(!xml.contains("OLD"), "{xml}");
    }
}
