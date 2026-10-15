package social.waddle.android.theme

import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import social.waddle.android.client.ConsistentColorHues
import kotlin.math.roundToInt

/**
 * XEP-0392 consistent colors, exactly as the web renders them:
 * `hsl(round(hue), 55%, 45%)` with the hue from the shared Rust
 * implementation (`consistentColor(name, 55, 45)` in `chat-ui.ts`).
 * Inputs are display names, verbatim.
 */

private val hues = ConsistentColorHues()

/**
 * The hue source. Production reads the shared Rust implementation; JVM
 * screenshot tests, which cannot load the native library, provide a
 * deterministic stand-in.
 */
val LocalConsistentHue = staticCompositionLocalOf<(String) -> Double> { hues::hue }

private const val CONSISTENT_SATURATION = 0.55f
private const val CONSISTENT_LIGHTNESS = 0.45f
private const val CONSISTENT_LIGHTNESS_ON_DARK = 0.7f
private const val CONSISTENT_TEXT_LIGHTNESS = 0.34f
private const val FULL_CIRCLE = 360

/**
 * [name]'s identity colour as TEXT on the current theme. The web's 45%
 * lightness is a fill colour: as text on the daylight ground a yellow or
 * green nick falls to ~2.5:1, so daylight text darkens it and night text
 * lifts it. Avatars keep the shared fill ([consistentColor] with
 * `onDark = false`).
 */
@Composable
@ReadOnlyComposable
fun consistentTextColor(name: String): Color {
    val dark = Waddle.colors.isDark
    return consistentColor(
        name,
        onDark = dark,
        hueOf = LocalConsistentHue.current,
        lightness = if (dark) CONSISTENT_LIGHTNESS_ON_DARK else CONSISTENT_TEXT_LIGHTNESS,
    )
}

/**
 * [name]'s identity colour adjusted for the background it sits on.
 * XEP-0392 §6 lets clients adapt lightness to the background; the web's
 * 45% reads well on daylight surfaces but sinks to ~3:1 on the night
 * ground, so night text lifts it while the hue (the identity) stays put.
 */
fun consistentColor(
    name: String,
    onDark: Boolean,
    hueOf: (String) -> Double = hues::hue,
    lightness: Float = if (onDark) CONSISTENT_LIGHTNESS_ON_DARK else CONSISTENT_LIGHTNESS,
): Color {
    val hue = ((hueOf(name).roundToInt() % FULL_CIRCLE) + FULL_CIRCLE) % FULL_CIRCLE
    return Color.hsl(hue.toFloat(), CONSISTENT_SATURATION, lightness)
}
