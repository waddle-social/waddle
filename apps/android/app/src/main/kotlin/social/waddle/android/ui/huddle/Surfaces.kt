package social.waddle.android.ui.huddle

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import social.waddle.android.theme.Waddle

/**
 * A Huddle card (web `card` recipe): radius 16, 1dp border, card colour.
 * [live] gives it the ember border and the live glow — for a huddle that
 * is happening, never for anything else.
 */
@Composable
fun HuddleCard(
    modifier: Modifier = Modifier,
    live: Boolean = false,
    onClick: (() -> Unit)? = null,
    contentPadding: PaddingValues = PaddingValues(Waddle.space.md),
    content: @Composable ColumnScope.() -> Unit,
) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(Waddle.radius.card)
    val border = BorderStroke(1.dp, if (live) colors.live else colors.border)
    val glowing = modifier.glow(
        color = colors.glowLive,
        blur = CARD_GLOW,
        cornerRadius = Waddle.radius.card,
        spread = CARD_GLOW_SPREAD,
        enabled = live,
    )
    val body: @Composable () -> Unit = {
        Column(modifier = Modifier.padding(contentPadding), content = content)
    }
    if (onClick != null) {
        Surface(
            onClick = onClick,
            modifier = glowing,
            shape = shape,
            color = colors.surface,
            border = border,
        ) { body() }
    } else {
        Surface(modifier = glowing, shape = shape, color = colors.surface, border = border) { body() }
    }
}

/** Which job a [PillButton] does, which decides its colour. */
enum class PillTone {
    /** Teal: the action on this screen. */
    PRIMARY,

    /** Ember: join something live. */
    LIVE,

    /** Outlined: a secondary way in ("Enter", "Show all"). */
    QUIET,

    /** Leave, remove, end. */
    DANGER,
}

/**
 * Huddle's button: a pill. 36dp tall (44dp touch target via Material's
 * minimum interactive size), control type.
 */
@Composable
fun PillButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    tone: PillTone = PillTone.PRIMARY,
    icon: ImageVector? = null,
    enabled: Boolean = true,
) {
    val colors = Waddle.colors
    val padding = PaddingValues(horizontal = Waddle.space.md)
    val content: @Composable RowScope.() -> Unit = {
        if (icon != null) {
            Icon(icon, contentDescription = null, modifier = Modifier.size(PILL_ICON))
            Box(Modifier.size(Waddle.space.xs))
        }
        Text(text = text, style = Waddle.type.control, maxLines = 1)
    }
    val sized = modifier.defaultMinSize(minHeight = Waddle.control.md)
    when (tone) {
        PillTone.QUIET -> OutlinedButton(
            onClick = onClick,
            modifier = sized,
            enabled = enabled,
            border = BorderStroke(1.dp, colors.border),
            colors = ButtonDefaults.outlinedButtonColors(contentColor = colors.fg),
            contentPadding = padding,
            content = content,
        )
        else -> Button(
            onClick = onClick,
            modifier = if (tone == PillTone.LIVE) {
                sized.glow(
                    color = colors.glowLive,
                    blur = PILL_GLOW,
                    cornerRadius = Waddle.radius.pill,
                    spread = PILL_GLOW_SPREAD,
                )
            } else {
                sized
            },
            enabled = enabled,
            colors = when (tone) {
                PillTone.LIVE -> ButtonDefaults.buttonColors(
                    containerColor = colors.live,
                    contentColor = colors.liveFg,
                )
                PillTone.DANGER -> ButtonDefaults.buttonColors(
                    containerColor = colors.danger,
                    contentColor = colors.dangerFg,
                )
                else -> ButtonDefaults.buttonColors(
                    containerColor = colors.action,
                    contentColor = colors.actionFg,
                )
            },
            contentPadding = padding,
            content = content,
        )
    }
}

/**
 * A loading placeholder bar (web `.chat-skeleton`: muted fill, radius 8,
 * opacity pulse 1 → .55 over 1.8s), with a little vertical air and a
 * minimum line height. Still under reduced motion.
 */
@Composable
fun SkeletonBlock(modifier: Modifier = Modifier, height: Dp = SKELETON_LINE, radius: Dp = Waddle.radius.tag) {
    SkeletonBox(
        modifier = modifier
            .padding(vertical = SKELETON_GAP)
            .defaultMinSize(minHeight = height),
        radius = radius,
    )
}

/**
 * The bare pulsing skeleton fill, sized entirely by [modifier]: avatar
 * discs, name bars and grid cells.
 */
@Composable
fun SkeletonBox(modifier: Modifier = Modifier, radius: Dp = Waddle.radius.tag) {
    val pulse = if (reducedMotion) {
        null
    } else {
        rememberInfiniteTransition(label = "skeleton").animateFloat(
            initialValue = 1f,
            targetValue = SKELETON_DIM,
            animationSpec = infiniteRepeatable(
                animation = tween(SKELETON_CYCLE_MILLIS / 2, easing = HuddleMotion.easing),
                repeatMode = RepeatMode.Reverse,
            ),
            label = "skeleton",
        )
    }
    Box(
        modifier = modifier
            // Read in the layer: the pulse redraws, never recomposes.
            .graphicsLayer { alpha = pulse?.value ?: 1f }
            .background(skeletonFill(), RoundedCornerShape(radius)),
    )
}

/**
 * Daylight's surface2 equals the daylight ground, so a skeleton on the bare
 * page would vanish; the border tone reads on both the ground and a card.
 */
@Composable
private fun skeletonFill(): Color = with(Waddle.colors) { if (isDark) surface2 else border }

private val CARD_GLOW = 28.dp
private val CARD_GLOW_SPREAD = (-10).dp
private val PILL_GLOW = 16.dp
private val PILL_GLOW_SPREAD = (-6).dp
private val PILL_ICON = 18.dp
private val SKELETON_LINE = 10.dp
private val SKELETON_GAP = 3.dp
private const val SKELETON_DIM = 0.55f
private const val SKELETON_CYCLE_MILLIS = 1800
