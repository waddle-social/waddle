package social.waddle.android.feature.conversation

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.StartOffset
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.HuddleMotion
import social.waddle.android.ui.huddle.reducedMotion

/**
 * Who is typing, above the composer (web `.typing-indicator`): up to
 * three overlapping 20dp faces, then a quiet teal pill with three
 * breathing teal dots and "Alice is typing". [avatarJidOf] resolves a
 * name to a real bare JID (`null` = initials). Slides away when nobody is.
 */
@Composable
fun TypingIndicator(
    names: List<String>,
    modifier: Modifier = Modifier,
    avatarJidOf: (String) -> String? = { null },
) {
    // Keep the last names while the row animates out.
    val shown = remember { mutableListHolder() }
    if (names.isNotEmpty()) shown.names = names
    AnimatedVisibility(
        visible = names.isNotEmpty(),
        enter = fadeIn() + expandVertically(),
        exit = fadeOut() + shrinkVertically(),
        modifier = modifier,
    ) {
        TypingRow(shown.names, avatarJidOf)
    }
}

private class NamesHolder(var names: List<String>)

private fun mutableListHolder() = NamesHolder(emptyList())

@Composable
private fun TypingRow(names: List<String>, avatarJidOf: (String) -> String?) {
    val colors = Waddle.colors
    val text = typingText(names)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xxs)
            .semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(-FACE_OVERLAP)) {
            names.take(MAX_TYPING_AVATARS).forEach { name ->
                key(name) {
                    PeerAvatar(
                        jid = avatarJidOf(name),
                        displayName = name,
                        size = FACE,
                        modifier = Modifier.border(FACE_RING, colors.bg, CircleShape),
                    )
                }
            }
        }
        val fill = colors.action.copy(alpha = PILL_TINT).compositeOver(colors.surface2)
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
            modifier = Modifier
                .defaultMinSize(minHeight = PILL_HEIGHT)
                .background(fill, CircleShape)
                .border(1.dp, colors.action.copy(alpha = PILL_BORDER), CircleShape)
                .padding(horizontal = Waddle.space.sm, vertical = Waddle.space.xxs),
        ) {
            TypingDots(colors.action)
            Text(
                text = text,
                style = MetaStyle,
                color = colors.fgMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** "X is typing", "X and Y are typing", "X and N others are typing". */
@Composable
private fun typingText(names: List<String>): String = when (names.size) {
    0 -> ""
    1 -> stringResource(R.string.typing_one, names[0])
    2 -> stringResource(R.string.typing_two, names[0], names[1])
    else -> pluralStringResource(R.plurals.typing_many, names.size - 1, names[0], names.size - 1)
}

/** Three 5dp teal dots breathing .85 → 1.15 over 1.25s, staggered; still under reduced motion. */
@Composable
private fun TypingDots(color: Color) {
    val scales: List<State<Float>> = if (reducedMotion) {
        remember { List(DOTS) { mutableFloatStateOf(1f) } }
    } else {
        val transition = rememberInfiniteTransition(label = "typing")
        List(DOTS) { dot ->
            transition.animateFloat(
                initialValue = DOT_MIN,
                targetValue = DOT_MAX,
                animationSpec = infiniteRepeatable(
                    animation = tween(DOT_CYCLE_MILLIS / 2, easing = HuddleMotion.easing),
                    repeatMode = RepeatMode.Reverse,
                    initialStartOffset = StartOffset(dot * DOT_STAGGER_MILLIS),
                ),
                label = "dot$dot",
            )
        }
    }
    Canvas(modifier = Modifier.size(width = DOTS_WIDTH, height = DOT * DOT_MAX)) {
        val radius = DOT.toPx() / 2f
        val step = (DOT + DOT_GAP).toPx()
        scales.forEachIndexed { index, scale ->
            // Read in the draw phase: the breathing redraws, never recomposes.
            drawCircle(
                color = color,
                radius = radius * scale.value,
                center = Offset(radius * DOT_MAX + index * step, size.height / 2f),
            )
        }
    }
}

private const val MAX_TYPING_AVATARS = 3
private const val DOTS = 3
private val FACE = 20.dp
private val FACE_OVERLAP = 6.dp
private val FACE_RING = 1.5.dp
private val PILL_HEIGHT = 24.dp
private val DOT = 5.dp
private val DOT_GAP = 3.dp
private val DOTS_WIDTH = DOT * DOTS + DOT_GAP * (DOTS - 1) + 2.dp
private const val DOT_MIN = 0.85f
private const val DOT_MAX = 1.15f
private const val DOT_CYCLE_MILLIS = 1250
private const val DOT_STAGGER_MILLIS = 160
private const val PILL_TINT = 0.08f
private const val PILL_BORDER = 0.16f
