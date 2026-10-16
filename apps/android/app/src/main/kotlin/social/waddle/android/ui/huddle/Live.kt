package social.waddle.android.ui.huddle

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.StartOffset
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.dp
import social.waddle.android.theme.Waddle

/**
 * The ember dot that marks something happening right now (web
 * `live-dot`: 8px, 10px ember glow). Static: the glow, not motion, says
 * live.
 */
@Composable
fun LiveDot(modifier: Modifier = Modifier) {
    val colors = Waddle.colors
    Box(
        modifier = modifier
            .size(LIVE_DOT)
            .glow(colors.glowLive, blur = LIVE_DOT_GLOW, cornerRadius = LIVE_DOT / 2)
            .background(colors.live, CircleShape),
    )
}

/**
 * Three ember bars beside a speaking person (web `.speaking-bars`: 2px
 * bars in a 12px box, scaleY .35 → 1 over 0.9s, staggered 150ms).
 * Reduced motion holds them still at .7.
 */
@Composable
fun SpeakingBars(modifier: Modifier = Modifier) {
    val color = Waddle.colors.live
    val scales = if (reducedMotion) {
        remember { List(BAR_COUNT) { mutableFloatStateOf(STILL_SCALE) } }
    } else {
        val transition = rememberInfiniteTransition(label = "speaking")
        List(BAR_COUNT) { bar ->
            transition.animateFloat(
                initialValue = MIN_SCALE,
                targetValue = 1f,
                animationSpec = infiniteRepeatable(
                    animation = tween(BAR_CYCLE_MILLIS / 2, easing = LinearEasing),
                    repeatMode = RepeatMode.Reverse,
                    initialStartOffset = StartOffset(bar * BAR_STAGGER_MILLIS),
                ),
                label = "bar$bar",
            )
        }
    }
    Canvas(
        modifier = modifier
            .size(width = BARS_WIDTH, height = BARS_HEIGHT)
            .clearAndSetSemantics { },
    ) {
        drawBars(scales, color)
    }
}

private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawBars(
    scales: List<State<Float>>,
    color: androidx.compose.ui.graphics.Color,
) {
    val barWidth = BAR_WIDTH.toPx()
    val gap = BAR_GAP.toPx()
    scales.forEachIndexed { index, scale ->
        // Read in the draw phase: the animation redraws, never recomposes.
        val height = size.height * scale.value
        drawRoundRect(
            color = color,
            topLeft = Offset(index * (barWidth + gap), size.height - height),
            size = Size(barWidth, height),
            cornerRadius = CornerRadius(barWidth / 2f),
        )
    }
}

private val LIVE_DOT = 8.dp
private val LIVE_DOT_GLOW = 10.dp
private const val BAR_COUNT = 3
private val BAR_WIDTH = 2.dp
private val BAR_GAP = 2.dp
private val BARS_WIDTH = BAR_WIDTH * BAR_COUNT + BAR_GAP * (BAR_COUNT - 1)
private val BARS_HEIGHT = 12.dp
private const val MIN_SCALE = 0.35f
private const val STILL_SCALE = 0.7f
private const val BAR_CYCLE_MILLIS = 900
private const val BAR_STAGGER_MILLIS = 150
