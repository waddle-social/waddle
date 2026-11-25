//! XEP-0392 nick colors, as on the web and the phones: the shared Rust
//! implementation (`waddle_xmpp_core::xep0392`, SHA-1 hue through HSLuv
//! `hsluvToRgb`) at saturation 100 and lightness 45, so one person is
//! the same color on every Waddle client.

use waddle_xmpp_core::xep0392::generate_color_with_params;

const SATURATION: f64 = 100.0;
const LIGHTNESS: f64 = 45.0;

/// `#rrggbb` for `identifier`, ready for Pango markup or CSS: a nickname
/// or, when the real address is known, the bare JID.
pub fn hex(identifier: &str) -> String {
    generate_color_with_params(identifier, SATURATION, LIGHTNESS).to_hex()
}

/// What a person's color is taken from: a room participant's nick (a nick
/// is the identity there), otherwise their bare JID when it is known.
pub fn identifier<'a>(in_room: bool, nick: &'a str, jid: Option<&'a str>) -> &'a str {
    if in_room {
        nick
    } else {
        jid.unwrap_or(nick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_color_is_hsluv_of_the_spec_hue() {
        // XEP-0392 test vector: "Romeo" is 327.255249 degrees; at
        // saturation 100 and lightness 50 HSLuv gives (0.865, 0, 0.686).
        assert!((waddle_xmpp_core::xep0392::compute_hue("Romeo") - 327.255_249).abs() < 1e-4);
        assert_eq!(
            generate_color_with_params("Romeo", 100.0, 50.0).to_hex(),
            "#dd00af"
        );
        assert_eq!(hex("Romeo").len(), 7);
    }

    #[test]
    fn rooms_color_by_nick_and_direct_messages_by_address() {
        assert_eq!(identifier(true, "ann", Some("ann@x.test")), "ann");
        assert_eq!(identifier(false, "ann", Some("ann@x.test")), "ann@x.test");
        assert_eq!(identifier(false, "ann", None), "ann");
    }

    #[test]
    fn the_same_identifier_always_gets_the_same_color() {
        assert_eq!(hex("juliet@capulet.lit"), hex("juliet@capulet.lit"));
        assert_ne!(hex("Romeo"), hex("council"));
        assert_eq!(hex("anyone").len(), 7);
    }
}
