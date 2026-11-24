//! XEP-0392 nick colors, as on the web and the phones: the shared Rust
//! hue (`waddle_xmpp_core::xep0392`) at HSL 55% saturation, 45%
//! lightness, so one person is the same color on every Waddle client.

use waddle_xmpp_core::xep0392::compute_hue;

const SATURATION: f64 = 0.55;
const LIGHTNESS: f64 = 0.45;

/// The sRGB bytes of the color for `identifier`: a nickname or, when the
/// real address is known, the bare JID.
pub fn rgb(identifier: &str) -> (u8, u8, u8) {
    hsl_to_rgb(compute_hue(identifier), SATURATION, LIGHTNESS)
}

/// `#rrggbb` for `identifier`, ready for Pango markup or CSS.
pub fn hex(identifier: &str) -> String {
    let (r, g, b) = rgb(identifier);
    format!("#{r:02x}{g:02x}{b:02x}")
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

/// HSL to sRGB, `hue` in degrees, the rest in `0.0..=1.0`.
fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let x = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector {
        s if s < 1.0 => (chroma, x, 0.0),
        s if s < 2.0 => (x, chroma, 0.0),
        s if s < 3.0 => (0.0, chroma, x),
        s if s < 4.0 => (0.0, x, chroma),
        s if s < 5.0 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let offset = lightness - chroma / 2.0;
    let byte = |channel: f64| ((channel + offset) * 255.0).round().clamp(0.0, 255.0) as u8;
    (byte(r), byte(g), byte(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_primaries_at_waddles_saturation_and_lightness() {
        // C = (1 - |2*0.45 - 1|) * 0.55 = 0.495; m = 0.45 - 0.2475 = 0.2025.
        assert_eq!(hsl_to_rgb(0.0, SATURATION, LIGHTNESS), (178, 52, 52));
        assert_eq!(hsl_to_rgb(120.0, SATURATION, LIGHTNESS), (52, 178, 52));
        assert_eq!(hsl_to_rgb(240.0, SATURATION, LIGHTNESS), (52, 52, 178));
    }

    #[test]
    fn the_hue_is_the_spec_vector() {
        // XEP-0392 test vector: "Romeo" is 327.255249 degrees.
        assert!((compute_hue("Romeo") - 327.255_249).abs() < 1e-4);
        assert_eq!(hex("Romeo"), {
            let (r, g, b) = hsl_to_rgb(327.255_249, SATURATION, LIGHTNESS);
            format!("#{r:02x}{g:02x}{b:02x}")
        });
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
