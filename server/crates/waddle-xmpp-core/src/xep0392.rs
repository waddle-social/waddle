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
//!    section "Angle generation").
//! 3. Map to a hue angle: `hue = (value / 65536) * 360`.
//! 4. Convert the hue to RGB with the HSLuv operation `hsluvToRgb`
//!    (section "RGB generation"), using an implementation-chosen
//!    saturation and lightness.
//!
//! The spec removed its Color Vision Deficiency correction in version
//! 0.8.0; clients that want less colour replace it with a static colour
//! instead (section "Accessibility Considerations").

use sha1::{Digest, Sha1};

/// Default saturation (0-100) for generated colors.
pub const DEFAULT_SATURATION: f64 = 100.0;

/// Default lightness (0-100) for generated colors.
pub const DEFAULT_LIGHTNESS: f64 = 50.0;

/// An HSLuv color value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HsluvColor {
    /// Hue angle in degrees (0.0-360.0).
    pub hue: f64,
    /// Saturation (0.0-100.0).
    pub saturation: f64,
    /// Lightness (0.0-100.0).
    pub lightness: f64,
}

impl HsluvColor {
    /// Create a new HSLuv color.
    #[must_use]
    pub fn new(hue: f64, saturation: f64, lightness: f64) -> Self {
        Self {
            hue,
            saturation,
            lightness,
        }
    }

    /// Convert to RGB tuple (0-255 each) with `hsluvToRgb`.
    #[must_use]
    pub fn to_rgb(&self) -> (u8, u8, u8) {
        let (r, g, b) = hsluv_to_rgb(self.hue, self.saturation, self.lightness);
        (to_byte(r), to_byte(g), to_byte(b))
    }

    /// Convert to an RGB hex string (e.g., `#ff8040`).
    #[must_use]
    pub fn to_hex(&self) -> String {
        let (r, g, b) = self.to_rgb();
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

fn to_byte(channel: f64) -> u8 {
    (channel * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Compute the consistent hue angle for an input string.
///
/// This is the core XEP-0392 algorithm:
/// 1. SHA-1 hash the input
/// 2. Take first 2 bytes as little-endian u16 (XEP-0392 "Angle
///    generation": "with the second byte being the most significant one")
/// 3. Map to 0.0-360.0 hue range
#[must_use]
pub fn compute_hue(input: &str) -> f64 {
    let hash = Sha1::digest(input.as_bytes());
    let value = u16::from_le_bytes([hash[0], hash[1]]);
    (value as f64 / 65536.0) * 360.0
}

/// Generate a consistent color for an input string.
///
/// Uses the default saturation (100) and lightness (50).
#[must_use]
pub fn generate_color(input: &str) -> HsluvColor {
    generate_color_with_params(input, DEFAULT_SATURATION, DEFAULT_LIGHTNESS)
}

/// Generate a consistent color with custom saturation and lightness
/// (both 0.0-100.0).
#[must_use]
pub fn generate_color_with_params(input: &str, saturation: f64, lightness: f64) -> HsluvColor {
    HsluvColor::new(compute_hue(input), saturation, lightness)
}

/// Trait for types that can generate a consistent color.
pub trait ConsistentColor {
    /// The string to use as input for color generation.
    fn color_input(&self) -> &str;

    /// Generate the consistent color for this entity.
    fn consistent_color(&self) -> HsluvColor {
        generate_color(self.color_input())
    }

    /// Generate the consistent color with custom parameters.
    fn consistent_color_with(&self, saturation: f64, lightness: f64) -> HsluvColor {
        generate_color_with_params(self.color_input(), saturation, lightness)
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

// ── HSLuv (reference algorithm, hsluv.org) ───────────────────────────

const REF_U: f64 = 0.197_830_006_642_836_8;
const REF_V: f64 = 0.468_319_994_938_791;
const KAPPA: f64 = 903.296_296_296_296_3;
const EPSILON: f64 = 0.008_856_451_679_035_631;

const M: [[f64; 3]; 3] = [
    [
        3.240_969_941_904_521_6,
        -1.537_383_177_570_093_7,
        -0.498_610_760_293_003_4,
    ],
    [
        -0.969_243_636_280_879_6,
        1.875_967_501_507_720_7,
        0.041_555_057_407_175_59,
    ],
    [
        0.055_630_079_696_993_66,
        -0.203_976_958_888_976_52,
        1.056_971_514_242_878_6,
    ],
];

/// The six lines bounding the sRGB gamut in LUV chroma space at
/// lightness `l`, as (slope, intercept) pairs.
fn gamut_lines(l: f64) -> [(f64, f64); 6] {
    let sub1 = (l + 16.0).powi(3) / 1_560_896.0;
    let sub2 = if sub1 > EPSILON { sub1 } else { l / KAPPA };
    let mut lines = [(0.0, 0.0); 6];
    for (c, row) in M.iter().enumerate() {
        let [m1, m2, m3] = *row;
        for t in 0..2 {
            let t_f = t as f64;
            let top1 = sub2 * (284_517.0 * m1 - 94_839.0 * m3);
            let top2 =
                sub2 * (838_422.0 * m3 + 769_860.0 * m2 + 731_718.0 * m1) * l - 769_860.0 * t_f * l;
            let bottom = sub2 * (632_260.0 * m3 - 126_452.0 * m2) + 126_452.0 * t_f;
            lines[c * 2 + t] = (top1 / bottom, top2 / bottom);
        }
    }
    lines
}

fn max_chroma_for_lh(l: f64, h_deg: f64) -> f64 {
    let h = h_deg.to_radians();
    let (sin, cos) = h.sin_cos();
    gamut_lines(l)
        .iter()
        .filter_map(|&(slope, intercept)| {
            // Distance from the origin to the line along the ray at angle h.
            let length = intercept / (sin - slope * cos);
            (length >= 0.0).then_some(length)
        })
        .fold(f64::MAX, f64::min)
}

fn from_linear(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn xyz_to_rgb(xyz: [f64; 3]) -> (f64, f64, f64) {
    let dot = |row: &[f64; 3]| row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2];
    (
        from_linear(dot(&M[0])),
        from_linear(dot(&M[1])),
        from_linear(dot(&M[2])),
    )
}

fn l_to_y(l: f64) -> f64 {
    if l <= 8.0 {
        l / KAPPA
    } else {
        ((l + 16.0) / 116.0).powi(3)
    }
}

fn luv_to_xyz(l: f64, u: f64, v: f64) -> [f64; 3] {
    if l == 0.0 {
        return [0.0, 0.0, 0.0];
    }
    let var_u = u / (13.0 * l) + REF_U;
    let var_v = v / (13.0 * l) + REF_V;
    let y = l_to_y(l);
    let x = 0.0 - (9.0 * y * var_u) / ((var_u - 4.0) * var_v - var_u * var_v);
    let z = (9.0 * y - (15.0 * var_v * y) - (var_v * x)) / (3.0 * var_v);
    [x, y, z]
}

/// The HSLuv operation `hsluvToRgb` from XEP-0392 "RGB generation".
///
/// `h` is a hue angle in degrees, `s` and `l` are 0.0-100.0. Returns
/// sRGB channels in `0.0..=1.0`.
#[must_use]
pub fn hsluv_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let (r, g, b) = if l > 99.999_999_9 {
        (1.0, 1.0, 1.0)
    } else if l < 0.000_000_01 {
        (0.0, 0.0, 0.0)
    } else {
        let h = h.rem_euclid(360.0);
        let chroma = max_chroma_for_lh(l, h) / 100.0 * s;
        let (sin, cos) = h.to_radians().sin_cos();
        xyz_to_rgb(luv_to_xyz(l, cos * chroma, sin * chroma))
    };
    (r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
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
        let color = generate_color_with_params("alice", 80.0, 40.0);
        assert_eq!(color.saturation, 80.0);
        assert_eq!(color.lightness, 40.0);
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
        let color = "alice".consistent_color_with(75.0, 60.0);
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

    /// XEP-0392 "Test Vectors" (testvectors-fullrange): the r, g, b
    /// columns at saturation 100 and lightness 50, printed to 3 places.
    #[test]
    fn test_spec_vector_rgb_hsluv() {
        let vectors = [
            ("Romeo", (0.865, 0.000, 0.686)),
            ("juliet@capulet.lit", (0.000, 0.515, 0.573)),
            ("\u{1F63A}", (0.872, 0.000, 0.659)),
            ("council", (0.918, 0.000, 0.394)),
            ("Board", (0.000, 0.527, 0.457)),
        ];
        for (input, (er, eg, eb)) in vectors {
            let c = generate_color(input);
            let (r, g, b) = hsluv_to_rgb(c.hue, c.saturation, c.lightness);
            for (got, want) in [(r, er), (g, eg), (b, eb)] {
                assert!(
                    (got - want).abs() < 0.001,
                    "{input:?}: rgb ({r:.3}, {g:.3}, {b:.3}) != ({er}, {eg}, {eb})"
                );
            }
        }
    }

    /// HSLuv endpoints and a spec vector rendered as hex.
    #[test]
    fn test_hsluv_reference_values() {
        assert_eq!(HsluvColor::new(0.0, 0.0, 100.0).to_rgb(), (255, 255, 255));
        assert_eq!(HsluvColor::new(0.0, 0.0, 0.0).to_rgb(), (0, 0, 0));
        // Romeo at S=100, L=50: spec vector (0.865, 0.000, 0.686).
        assert_eq!(generate_color("Romeo").to_hex(), "#dd00af");
    }

    /// HSLuv is not HSL: pure HSL red must not come out for hue 0.
    #[test]
    fn test_hsluv_differs_from_hsl() {
        assert_ne!(HsluvColor::new(0.0, 100.0, 50.0).to_hex(), "#ff0000");
    }

    #[test]
    fn test_hsluv_channels_stay_in_gamut() {
        for hue in (0..360).step_by(7) {
            for l in [5.0, 34.0, 50.0, 70.0, 95.0] {
                let (r, g, b) = hsluv_to_rgb(f64::from(hue), 100.0, l);
                for c in [r, g, b] {
                    assert!((0.0..=1.0).contains(&c));
                }
            }
        }
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
