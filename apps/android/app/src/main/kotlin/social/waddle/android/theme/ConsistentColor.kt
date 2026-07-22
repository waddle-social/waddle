package social.waddle.android.theme

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

private const val CONSISTENT_SATURATION = 0.55f
private const val CONSISTENT_LIGHTNESS = 0.45f
private const val FULL_CIRCLE = 360

/** The stable identity color for [name] (author nick, peer name, …). */
fun consistentColor(name: String): Color {
    val hue = ((hues.hue(name).roundToInt() % FULL_CIRCLE) + FULL_CIRCLE) % FULL_CIRCLE
    return Color.hsl(hue.toFloat(), CONSISTENT_SATURATION, CONSISTENT_LIGHTNESS)
}
