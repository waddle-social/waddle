package social.waddle.android.ui.huddle

import android.content.Context
import android.provider.Settings
import androidx.compose.animation.core.FiniteAnimationSpec
import androidx.compose.animation.core.tween
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import social.waddle.android.theme.WaddleTokens

/**
 * Huddle motion: 160ms ease-out for state changes, 240ms for movement,
 * 400ms for a card entering. Speaking bars and live pulses are the only
 * loops; [LocalReducedMotion] turns every loop and glow pulse off.
 */
object HuddleMotion {
    const val FAST = WaddleTokens.Huddle.DurationMillis.FAST
    const val NORMAL = WaddleTokens.Huddle.DurationMillis.NORMAL
    const val ENTER = WaddleTokens.Huddle.DurationMillis.ENTER
    val easing = WaddleTokens.Huddle.Easing.out

    fun <T> fast(): FiniteAnimationSpec<T> = tween(FAST, easing = easing)

    fun <T> normal(): FiniteAnimationSpec<T> = tween(NORMAL, easing = easing)

    fun <T> enter(): FiniteAnimationSpec<T> = tween(ENTER, easing = easing)
}

/**
 * Whether the user asked for less motion: Android's "Remove animations"
 * sets the animator duration scale to 0. Provided once at the app root
 * ([reducedMotionOf]) and overridable in tests and screenshots.
 */
val LocalReducedMotion = staticCompositionLocalOf { false }

/** True when system animations are off. Read on resume by the host. */
fun reducedMotionOf(context: Context): Boolean = runCatching {
    Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
}.getOrDefault(false)

val reducedMotion: Boolean
    @Composable
    @ReadOnlyComposable
    get() = LocalReducedMotion.current
