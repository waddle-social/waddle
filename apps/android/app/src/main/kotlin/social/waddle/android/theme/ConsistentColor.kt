package social.waddle.android.theme

import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import social.waddle.android.client.ConsistentColorHexes

/**
 * XEP-0392 consistent colors, exactly as the web renders them: the
 * shared Rust implementation (SHA-1 hue through HSLuv `hsluvToRgb`) at
 * saturation 100 and lightness 45 (`consistentColor(name, 100, 45)` in
 * `chat-ui.ts`). Inputs are display names, verbatim.
 */

private val hexes = ConsistentColorHexes()

/**
 * The color source: `(name, lightness) -> #rrggbb`. Production reads the
 * shared Rust implementation; JVM screenshot tests, which cannot load the
 * native library, provide a deterministic stand-in.
 */
val LocalConsistentHex = staticCompositionLocalOf<(String, Double) -> String> { ::sharedHex }

private fun sharedHex(name: String, lightness: Double): String = hexes.hex(name, CONSISTENT_SATURATION, lightness)

private const val CONSISTENT_SATURATION = 100.0
private const val CONSISTENT_LIGHTNESS = 45.0
private const val CONSISTENT_LIGHTNESS_ON_DARK = 70.0
private const val CONSISTENT_TEXT_LIGHTNESS = 40.0
private const val RGB_MASK = 0xFFFFFFL
private const val OPAQUE = 0xFF000000L
private const val HEX_RADIX = 16

/**
 * [name]'s identity colour as TEXT on the current theme. The fill
 * lightness is too light for text on the daylight ground, so daylight
 * text darkens it and night text lifts it. Avatars keep the shared fill
 * ([consistentColor] with `onDark = false`).
 */
@Composable
@ReadOnlyComposable
fun consistentTextColor(name: String): Color {
    val dark = Waddle.colors.isDark
    return consistentColor(
        name,
        onDark = dark,
        hexOf = LocalConsistentHex.current,
        lightness = if (dark) CONSISTENT_LIGHTNESS_ON_DARK else CONSISTENT_TEXT_LIGHTNESS,
    )
}

/**
 * [name]'s identity colour adjusted for the background it sits on.
 * XEP-0392 lets clients adapt lightness to the background; night text
 * lifts it while the hue (the identity) stays put.
 */
fun consistentColor(
    name: String,
    onDark: Boolean,
    hexOf: (String, Double) -> String = ::sharedHex,
    lightness: Double = if (onDark) CONSISTENT_LIGHTNESS_ON_DARK else CONSISTENT_LIGHTNESS,
): Color {
    val rgb = hexOf(name, lightness).removePrefix("#").toLong(HEX_RADIX) and RGB_MASK
    return Color((OPAQUE or rgb).toInt())
}
