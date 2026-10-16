package social.waddle.android.feature.call

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.indication
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.ripple
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.core.view.WindowCompat
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleTheme
import social.waddle.android.ui.huddle.HuddleMotion
import social.waddle.android.ui.huddle.reducedMotion
import social.waddle.android.ui.huddle.ring

/**
 * The call stage is always night, whatever the app theme: a live call
 * is lit like a room at night (Huddle "night is home"; toasts stay night
 * for the same reason). Swaps in the night palette for [content] and
 * turns the system bar icons light while it shows.
 */
@Composable
internal fun NightStage(content: @Composable () -> Unit) {
    LightSystemBarIcons(statusBar = true, navigationBar = true)
    WaddleTheme(themeMode = ThemeMode.NIGHT, content = content)
}

/**
 * Light status/navigation bar icons while a night surface sits behind
 * them; restores the app theme's choice when it leaves. Must be called
 * OUTSIDE the night theme: it reads the app's own theme to restore.
 */
@Composable
internal fun LightSystemBarIcons(statusBar: Boolean, navigationBar: Boolean) {
    val appIsDark = Waddle.colors.isDark
    val view = LocalView.current
    DisposableEffect(view, appIsDark, statusBar, navigationBar) {
        val window = view.context.findActivity()?.window
        val controller = window?.let { WindowCompat.getInsetsController(it, view) }
        if (statusBar) controller?.isAppearanceLightStatusBars = false
        if (navigationBar) controller?.isAppearanceLightNavigationBars = false
        onDispose {
            if (statusBar) controller?.isAppearanceLightStatusBars = !appIsDark
            if (navigationBar) controller?.isAppearanceLightNavigationBars = !appIsDark
        }
    }
}

private tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}

/** What the ring around a stage avatar says. */
internal enum class StageRing {
    /** Not connected yet: no ring. */
    NONE,

    /** In the huddle: teal ring. */
    HUDDLE,

    /** Speaking: ember ring with the live glow. */
    SPEAKING,

    /** Ringing us: ember ring with slow waves going out; reduced motion keeps only the ring. */
    RINGING,

    /** We are calling them: no ring yet, quiet neutral waves (not ember: nothing is live). */
    CALLING,
}

/**
 * The large avatar at the centre of a call: [HuddleAvatar][social.waddle.android.ui.huddle.HuddleAvatar]'s
 * ring and glow at stage scale, with the ring easing between teal and
 * ember instead of snapping.
 */
@Composable
internal fun StageAvatar(
    jid: String?,
    name: String,
    size: Dp,
    ring: StageRing,
    modifier: Modifier = Modifier,
) {
    val colors = Waddle.colors
    val still = reducedMotion
    val ringTarget = when (ring) {
        StageRing.NONE, StageRing.CALLING -> Color.Transparent
        StageRing.HUDDLE -> colors.action
        StageRing.SPEAKING, StageRing.RINGING -> colors.live
    }
    val ringColor by animateColorAsState(
        targetValue = ringTarget,
        animationSpec = if (still) snap() else HuddleMotion.fast(),
        label = "stage-ring",
    )
    val glowing = ring == StageRing.SPEAKING || ring == StageRing.RINGING
    val waving = ring == StageRing.RINGING || ring == StageRing.CALLING
    val waves = if (waving && !still) {
        rememberInfiniteTransition(label = "ringing").animateFloat(
            initialValue = 0f,
            targetValue = 1f,
            animationSpec = infiniteRepeatable(
                animation = tween(RINGING_WAVE_MILLIS, easing = HuddleMotion.easing),
                repeatMode = RepeatMode.Restart,
            ),
            label = "ringing-wave",
        )
    } else {
        null
    }
    val waveColor = if (ring == StageRing.RINGING) colors.live else colors.fgSoft
    Box(
        modifier = modifier
            .size(size)
            .drawBehind {
                // Read in the draw phase: the waves redraw, never recompose.
                val progress = waves?.value ?: return@drawBehind
                val base = this.size.minDimension / 2f + (STAGE_RING_GAP + STAGE_RING_WIDTH).toPx()
                repeat(RINGING_WAVES) { index ->
                    val t = (progress + index.toFloat() / RINGING_WAVES) % 1f
                    drawCircle(
                        color = waveColor.copy(alpha = RINGING_WAVE_ALPHA * (1f - t)),
                        radius = base + t * RINGING_WAVE_REACH.toPx(),
                        style = Stroke(width = STAGE_RING_WIDTH.toPx()),
                    )
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        PeerAvatar(
            jid = jid,
            displayName = name,
            size = size,
            modifier = if (ringColor.alpha == 0f) {
                Modifier
            } else {
                Modifier.ring(
                    gapColor = colors.bg,
                    ringColor = ringColor,
                    gap = STAGE_RING_GAP,
                    width = STAGE_RING_WIDTH,
                    glow = if (glowing) colors.glowLive else null,
                    glowBlur = STAGE_GLOW,
                )
            },
        )
    }
}

/** What a [CallControl] does, which decides its colour. */
internal enum class ControlTone {
    /** Toggles and plain actions: quiet until engaged, then lit. */
    NEUTRAL,

    /** Hand raised: gold when engaged (attention, not live). */
    GOLD,

    /** Leave, end, decline. */
    DANGER,

    /** Answer: the teal action. */
    ACTION,
}

/**
 * A round call control with its name under it. [engaged] makes it a
 * switch (`null` for a plain button): engaged controls invert to a lit
 * fill so "muted" or "speaker on" reads from across the room, and
 * screen readers hear the name plus on/off. The whole column is the
 * touch target; the ripple stays on the circle.
 */
@Composable
internal fun CallControl(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    tone: ControlTone = ControlTone.NEUTRAL,
    engaged: Boolean? = null,
    size: Dp = CALL_CONTROL,
    enabled: Boolean = true,
) {
    val colors = Waddle.colors
    val lit = engaged == true
    val (fill, ink) = when {
        tone == ControlTone.DANGER -> colors.danger to colors.dangerFg
        tone == ControlTone.ACTION -> colors.action to colors.actionFg
        lit && tone == ControlTone.GOLD -> colors.gold to colors.bg
        lit -> colors.fg to colors.bg
        else -> colors.surface2 to colors.fg
    }
    val still = reducedMotion
    val animatedFill by animateColorAsState(fill, if (still) snap() else HuddleMotion.fast(), label = "control-fill")
    val animatedInk by animateColorAsState(ink, if (still) snap() else HuddleMotion.fast(), label = "control-ink")
    val interaction = remember { MutableInteractionSource() }
    val action = if (engaged != null) {
        Modifier.toggleable(
            value = engaged,
            enabled = enabled,
            role = Role.Switch,
            interactionSource = interaction,
            indication = null,
            onValueChange = { onClick() },
        )
    } else {
        Modifier.clickable(
            enabled = enabled,
            role = Role.Button,
            interactionSource = interaction,
            indication = null,
            onClick = onClick,
        )
    }
    Column(
        modifier = modifier
            .then(action)
            .alpha(if (enabled) 1f else DISABLED_ALPHA),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xs),
    ) {
        Box(
            modifier = Modifier
                .size(size)
                .background(animatedFill, CircleShape)
                .then(
                    if (!lit && (tone == ControlTone.NEUTRAL || tone == ControlTone.GOLD)) {
                        Modifier.border(1.dp, colors.border, CircleShape)
                    } else {
                        Modifier
                    },
                )
                .indication(interaction, ripple(bounded = true, radius = size / 2)),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = icon,
                contentDescription = null,
                tint = animatedInk,
                modifier = Modifier.size(if (size > CALL_CONTROL) LARGE_ICON else ICON),
            )
        }
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            color = if (lit) colors.fg else colors.fgMuted,
            textAlign = TextAlign.Center,
            maxLines = 2,
        )
    }
}

/** In-call control size: a 56dp circle (Huddle calls brief: 56–72). */
internal val CALL_CONTROL = 56.dp

/** Answer and decline on the ring: the largest targets in the app. */
internal val CALL_CONTROL_LARGE = 72.dp

private val ICON = 24.dp
private val LARGE_ICON = 32.dp
private const val DISABLED_ALPHA = 0.38f
private val STAGE_RING_GAP = 4.dp
private val STAGE_RING_WIDTH = 3.dp
private val STAGE_GLOW = 32.dp
private const val RINGING_WAVE_MILLIS = 2_400
private const val RINGING_WAVES = 2
private const val RINGING_WAVE_ALPHA = 0.45f
private val RINGING_WAVE_REACH = 24.dp
