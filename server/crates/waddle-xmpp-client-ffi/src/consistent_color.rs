//! XEP-0392 Consistent Color Generation hue export.
//!
//! Native clients color avatars/nicks with the same SHA-1 based hue
//! the web client gets from the WASM `xep0392_consistent_hue` export,
//! so a user renders identically across every Waddle surface.
//! Saturation/lightness stay a client-side presentation choice
//! (Waddle renders HSL 55%/45%).

/// XEP-0392 §"Angle generation": hue in degrees (`0.0..360.0`) for
/// `input` (nickname or bare JID, used verbatim — no normalization).
#[uniffi::export]
pub fn consistent_color_hue(input: String) -> f64 {
    waddle_xmpp_core::xep0392::compute_hue(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// XEP-0392 §"Test Vectors" — the export must match the core
    /// implementation the web WASM build ships.
    #[test]
    fn matches_spec_vectors() {
        let vectors = [
            ("Romeo", 327.255_249),
            ("juliet@capulet.lit", 209.410_400),
            ("\u{1F63A}", 331.199_341),
            ("council", 359.994_507),
            ("Board", 171.430_664),
        ];
        for (input, expected) in vectors {
            let hue = consistent_color_hue(input.to_string());
            assert!(
                (hue - expected).abs() < 1e-4,
                "consistent_color_hue({input:?}) = {hue}, expected {expected}"
            );
        }
    }
}
