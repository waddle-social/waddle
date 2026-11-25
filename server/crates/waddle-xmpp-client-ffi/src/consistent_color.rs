//! XEP-0392 Consistent Color Generation export.
//!
//! Native clients color avatars/nicks with the same SHA-1 hue and HSLuv
//! conversion the web client gets from the WASM `xep0392_consistent_color`
//! export, so a user renders identically across every Waddle surface.
//! Saturation/lightness stay a client-side presentation choice.

/// XEP-0392: the `#rrggbb` color for `input` (nickname or bare JID, used
/// verbatim, no normalization): SHA-1 hue (section "Angle generation")
/// converted with HSLuv `hsluvToRgb` (section "RGB generation") at
/// `saturation` and `lightness` (0.0-100.0).
#[uniffi::export]
pub fn consistent_color_hex(input: String, saturation: f64, lightness: f64) -> String {
    waddle_xmpp_core::xep0392::generate_color_with_params(&input, saturation, lightness).to_hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// XEP-0392 "Test Vectors": the export must match the spec's RGB
    /// columns at saturation 100 and lightness 50.
    #[test]
    fn matches_spec_vectors() {
        let vectors = [
            ("Romeo", [0.865, 0.000, 0.686]),
            ("juliet@capulet.lit", [0.000, 0.515, 0.573]),
            ("\u{1F63A}", [0.872, 0.000, 0.659]),
            ("council", [0.918, 0.000, 0.394]),
            ("Board", [0.000, 0.527, 0.457]),
        ];
        for (input, expected) in vectors {
            let hex = consistent_color_hex(input.to_string(), 100.0, 50.0);
            for (i, want) in expected.iter().enumerate() {
                let byte = u8::from_str_radix(&hex[1 + 2 * i..3 + 2 * i], 16).unwrap_or(0);
                assert!(
                    (f64::from(byte) / 255.0 - want).abs() < 0.003,
                    "consistent_color_hex({input:?}) = {hex}, expected channel {i} near {want}"
                );
            }
        }
    }
}
