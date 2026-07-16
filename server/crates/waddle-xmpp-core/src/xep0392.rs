//! XEP-0392: Consistent Color Generation
//!
//! Generates deterministic colors from input strings (JIDs, nicknames)
//! so that each user gets a unique, consistent color across clients.
//!
//! ## Algorithm
//!
//! 1. Compute SHA-1 hash of the input string (UTF-8 encoded).
//! 2. Take the first two bytes of the hash as a little-endian u16
//!    (the second byte is the most significant one, per XEP-0392
//!    §"Angle generation").
//! 3. Map to a hue angle: `hue = (value / 65536) * 360`.
//! 4. Combine with configurable saturation and lightness to produce HSL.
//!
//! ## Color Vision Deficiency Correction
//!
//! The spec defines optional corrections for red-green and blue-yellow
//! deficiencies by remapping the hue range. This module supports both.

use sha1::{Digest, Sha1};

/// Default saturation percentage for generated colors.
pub const DEFAULT_SATURATION: f64 = 100.0;

/// Default lightness percentage for generated colors.
pub const DEFAULT_LIGHTNESS: f64 = 50.0;

/// Color vision deficiency correction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CvdCorrection {
    /// No correction (full 360° hue range).
    None,
    /// Red-green deficiency (deuteranopia/protanopia).
    /// Maps hue to two safe ranges: blue and yellow.
    RedGreen,
    /// Blue-yellow deficiency (tritanopia).
    /// Maps hue to two safe ranges: red and cyan.
    BlueYellow,
}

/// An HSL color value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HslColor {
    /// Hue angle in degrees (0.0–360.0).
    pub hue: f64,
    /// Saturation percentage (0.0–100.0).
    pub saturation: f64,
    /// Lightness percentage (0.0–100.0).
    pub lightness: f64,
}

impl HslColor {
    /// Create a new HSL color.
    pub fn new(hue: f64, saturation: f64, lightness: f64) -> Self {
        Self {
            hue,
            saturation,
            lightness,
        }
    }

    /// Convert to a CSS `hsl()` string.
    pub fn to_css(&self) -> String {
        format!(
            "hsl({:.0}, {:.0}%, {:.0}%)",
            self.hue, self.saturation, self.lightness
        )
    }

    /// Convert to an RGB hex string (e.g., `#ff8040`).
    pub fn to_hex(&self) -> String {
        let (r, g, b) = hsl_to_rgb(self.hue, self.saturation / 100.0, self.lightness / 100.0);
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    }

    /// Convert to RGB tuple (0–255 each).
    pub fn to_rgb(&self) -> (u8, u8, u8) {
        hsl_to_rgb(self.hue, self.saturation / 100.0, self.lightness / 100.0)
    }
}

/// Compute the consistent hue angle for an input string.
///
/// This is the core XEP-0392 algorithm:
/// 1. SHA-1 hash the input
/// 2. Take first 2 bytes as little-endian u16 (XEP-0392 §"Angle
///    generation": "with the second byte being the most significant one")
/// 3. Map to 0.0–360.0 hue range
pub fn compute_hue(input: &str) -> f64 {
    let hash = Sha1::digest(input.as_bytes());
    let value = u16::from_le_bytes([hash[0], hash[1]]);
    (value as f64 / 65536.0) * 360.0
}

/// Apply color vision deficiency correction to a hue angle.
pub fn apply_cvd_correction(hue: f64, correction: CvdCorrection) -> f64 {
    match correction {
        CvdCorrection::None => hue,
        CvdCorrection::RedGreen => {
            // Map full range to blue (180°–300°) region
            180.0 + (hue / 360.0) * 120.0
        }
        CvdCorrection::BlueYellow => {
            // Map full range to red-green (0°–180°) region
            (hue / 360.0) * 180.0
        }
    }
}

/// Generate a consistent color for an input string.
///
/// Uses the default saturation (100%) and lightness (50%).
pub fn generate_color(input: &str) -> HslColor {
    generate_color_with_params(
        input,
        DEFAULT_SATURATION,
        DEFAULT_LIGHTNESS,
        CvdCorrection::None,
    )
}

/// Generate a consistent color with custom parameters.
pub fn generate_color_with_params(
    input: &str,
    saturation: f64,
    lightness: f64,
    cvd: CvdCorrection,
) -> HslColor {
    let hue = apply_cvd_correction(compute_hue(input), cvd);
    HslColor::new(hue, saturation, lightness)
}

/// Trait for types that can generate a consistent color.
pub trait ConsistentColor {
    /// The string to use as input for color generation.
    fn color_input(&self) -> &str;

    /// Generate the consistent color for this entity.
    fn consistent_color(&self) -> HslColor {
        generate_color(self.color_input())
    }

    /// Generate the consistent color with custom parameters.
    fn consistent_color_with(
        &self,
        saturation: f64,
        lightness: f64,
        cvd: CvdCorrection,
    ) -> HslColor {
        generate_color_with_params(self.color_input(), saturation, lightness, cvd)
    }
}

impl ConsistentColor for str {
    fn color_input(&self) -> &str {
        self
    }
}

impl ConsistentColor for String {
    fn color_input(&self) -> &str {
        self
    }
}

// ── HSL to RGB conversion ────────────────────────────────────────────

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    let r = hue_to_rgb(p, q, h / 360.0 + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h / 360.0);
    let b = hue_to_rgb(p, q, h / 360.0 - 1.0 / 3.0);

    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hue_deterministic() {
        let hue1 = compute_hue("alice@example.com");
        let hue2 = compute_hue("alice@example.com");
        assert_eq!(hue1, hue2);
    }

    #[test]
    fn test_compute_hue_different_inputs() {
        let hue_alice = compute_hue("alice@example.com");
        let hue_bob = compute_hue("bob@example.com");
        assert_ne!(hue_alice, hue_bob);
    }

    #[test]
    fn test_compute_hue_range() {
        for input in ["alice", "bob", "charlie", "dave", "eve", "frank", ""] {
            let hue = compute_hue(input);
            assert!(
                (0.0..360.0).contains(&hue),
                "hue {hue} out of range for input {input:?}"
            );
        }
    }

    #[test]
    fn test_generate_color_defaults() {
        let color = generate_color("alice@example.com");
        assert_eq!(color.saturation, DEFAULT_SATURATION);
        assert_eq!(color.lightness, DEFAULT_LIGHTNESS);
        assert!((0.0..360.0).contains(&color.hue));
    }

    #[test]
    fn test_generate_color_custom_params() {
        let color = generate_color_with_params("alice", 80.0, 40.0, CvdCorrection::None);
        assert_eq!(color.saturation, 80.0);
        assert_eq!(color.lightness, 40.0);
    }

    #[test]
    fn test_cvd_red_green() {
        let normal = generate_color("test");
        let corrected = generate_color_with_params(
            "test",
            DEFAULT_SATURATION,
            DEFAULT_LIGHTNESS,
            CvdCorrection::RedGreen,
        );
        // Red-green maps to 180°–300° range
        assert!(corrected.hue >= 180.0 && corrected.hue <= 300.0);
        // Should differ from uncorrected (usually)
        assert_ne!(normal.hue, corrected.hue);
    }

    #[test]
    fn test_cvd_blue_yellow() {
        let corrected = generate_color_with_params(
            "test",
            DEFAULT_SATURATION,
            DEFAULT_LIGHTNESS,
            CvdCorrection::BlueYellow,
        );
        // Blue-yellow maps to 0°–180° range
        assert!(corrected.hue >= 0.0 && corrected.hue <= 180.0);
    }

    #[test]
    fn test_hsl_to_css() {
        let color = HslColor::new(120.0, 100.0, 50.0);
        assert_eq!(color.to_css(), "hsl(120, 100%, 50%)");
    }

    #[test]
    fn test_hsl_to_hex() {
        // Pure red
        let red = HslColor::new(0.0, 100.0, 50.0);
        assert_eq!(red.to_hex(), "#ff0000");

        // Pure green
        let green = HslColor::new(120.0, 100.0, 50.0);
        assert_eq!(green.to_hex(), "#00ff00");

        // Pure blue
        let blue = HslColor::new(240.0, 100.0, 50.0);
        assert_eq!(blue.to_hex(), "#0000ff");
    }

    #[test]
    fn test_hsl_to_rgb() {
        let white = HslColor::new(0.0, 0.0, 100.0);
        assert_eq!(white.to_rgb(), (255, 255, 255));

        let black = HslColor::new(0.0, 0.0, 0.0);
        assert_eq!(black.to_rgb(), (0, 0, 0));

        let gray = HslColor::new(0.0, 0.0, 50.0);
        assert_eq!(gray.to_rgb(), (128, 128, 128));
    }

    #[test]
    fn test_consistent_color_trait() {
        let color = "alice@example.com".consistent_color();
        assert!((0.0..360.0).contains(&color.hue));

        let s = String::from("bob@example.com");
        let color2 = s.consistent_color();
        assert!((0.0..360.0).contains(&color2.hue));
    }

    #[test]
    fn test_consistent_color_with_params() {
        let color = "alice".consistent_color_with(75.0, 60.0, CvdCorrection::None);
        assert_eq!(color.saturation, 75.0);
        assert_eq!(color.lightness, 60.0);
    }

    /// XEP-0392 §"Test Vectors" (testvectors-fullrange): angle column.
    fn assert_spec_vector(input: &str, expected_hue: f64) {
        let hue = compute_hue(input);
        assert!(
            (hue - expected_hue).abs() < 1e-4,
            "compute_hue({input:?}) = {hue}, expected {expected_hue}"
        );
    }

    #[test]
    fn test_spec_vector_romeo() {
        assert_spec_vector("Romeo", 327.255_249);
    }

    #[test]
    fn test_spec_vector_juliet_jid() {
        assert_spec_vector("juliet@capulet.lit", 209.410_400);
    }

    #[test]
    fn test_spec_vector_cat_emoji() {
        assert_spec_vector("\u{1F63A}", 331.199_341);
    }

    #[test]
    fn test_spec_vector_council() {
        assert_spec_vector("council", 359.994_507);
    }

    #[test]
    fn test_spec_vector_board() {
        assert_spec_vector("Board", 171.430_664);
    }

    #[test]
    fn test_empty_string() {
        let color = generate_color("");
        assert!((0.0..360.0).contains(&color.hue));
    }

    #[test]
    fn test_unicode_input() {
        let color = generate_color("日本語ユーザー");
        assert!((0.0..360.0).contains(&color.hue));
    }
}
