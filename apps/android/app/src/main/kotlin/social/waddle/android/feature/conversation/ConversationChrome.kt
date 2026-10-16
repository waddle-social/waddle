package social.waddle.android.feature.conversation

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Paint
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.theme.WaddleTokens
import social.waddle.android.ui.huddle.HuddleMotion
import social.waddle.android.ui.huddle.reducedMotion

/**
 * Huddle's overlay shadow (web `shadows.overlay`: 0 16px 40px, 45% black)
 * under floating things that sit over the timeline: the "Latest" pill and
 * composer popovers. Drawn as a blurred shadow of the element's own
 * rounded shape, outside its bounds, so it costs no layout.
 */
fun Modifier.overlayShadow(cornerRadius: Dp): Modifier = drawBehind {
    val shadow = WaddleTokens.Huddle.Shadow.overlay
    val radius = cornerRadius.toPx()
    drawIntoCanvas { canvas ->
        val paint = Paint()
        paint.asFrameworkPaint().apply {
            isAntiAlias = true
            color = android.graphics.Color.TRANSPARENT
            setShadowLayer(shadow.blur.toPx() / 2f, shadow.offsetX.toPx(), shadow.offsetY.toPx(), shadow.color.toArgb())
        }
        canvas.drawRoundRect(0f, 0f, size.width, size.height, radius, radius, paint)
    }
}

/**
 * One sheet chrome for every conversation sheet: card colour, radius-16
 * top corners, a 40×4 handle, an optional Space Grotesk title that is a
 * heading for screen readers, and 16dp side padding. Insets come from the
 * sheet's own window insets (navigation bar and keyboard), never added
 * again by the content.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationSheet(
    onDismissRequest: () -> Unit,
    modifier: Modifier = Modifier,
    title: String? = null,
    contentPadding: Dp = Waddle.space.md,
    content: @Composable ColumnScope.() -> Unit,
) {
    val colors = Waddle.colors
    ModalBottomSheet(
        onDismissRequest = onDismissRequest,
        modifier = modifier,
        sheetState = rememberModalBottomSheetState(),
        shape = RoundedCornerShape(topStart = Waddle.radius.card, topEnd = Waddle.radius.card),
        containerColor = colors.surface,
        contentColor = colors.fg,
        tonalElevation = 0.dp,
        dragHandle = { SheetHandle() },
    ) {
        SheetBody(title = title, contentPadding = contentPadding, content = content)
    }
}

/** The sheet's content column: title, then content, with the bottom breathing room. */
@Composable
internal fun SheetBody(
    title: String?,
    contentPadding: Dp = Waddle.space.md,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(modifier = Modifier.padding(bottom = Waddle.space.md)) {
        if (title != null) SheetTitle(title, Modifier.padding(horizontal = contentPadding))
        Column(
            modifier = Modifier.padding(horizontal = contentPadding),
            verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            content = content,
        )
    }
}

/** The 40×4 grab handle (web action sheet). */
@Composable
internal fun SheetHandle(modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .fillMaxWidth()
            .padding(top = Waddle.space.sm, bottom = Waddle.space.xs),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(width = HANDLE_WIDTH, height = HANDLE_HEIGHT)
                .background(Waddle.colors.border, CircleShape),
        )
    }
}

/** A sheet title: Space Grotesk 18/600, a heading. */
@Composable
internal fun SheetTitle(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = Waddle.type.title,
        color = Waddle.colors.fg,
        modifier = modifier
            .fillMaxWidth()
            .padding(bottom = Waddle.space.sm)
            .semantics { heading() },
    )
}

/**
 * A floating panel over the timeline (mention and slash popovers): card
 * colour, a 1dp border and the overlay shadow, so it reads as lifted off
 * the ground in both themes.
 */
@Composable
internal fun PopoverSurface(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    val shape = RoundedCornerShape(Waddle.radius.panel)
    Surface(
        shape = shape,
        color = Waddle.colors.surface,
        contentColor = Waddle.colors.fg,
        border = BorderStroke(1.dp, Waddle.colors.border),
        modifier = modifier
            .fillMaxWidth()
            .padding(bottom = Waddle.space.xs)
            .overlayShadow(Waddle.radius.panel),
    ) {
        Column(content = content)
    }
}

/**
 * Quiet is an invitation: the mascot, a Space Grotesk title (a heading)
 * and a line that asks someone to start. Used where a conversation has
 * nothing in it yet.
 */
@Composable
internal fun ConversationEmptyState(title: String, body: String, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier
            .widthIn(max = EMPTY_MAX_WIDTH)
            .padding(horizontal = Waddle.space.lg),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xs),
    ) {
        Mascot()
        Text(
            text = title,
            style = Waddle.type.title,
            color = Waddle.colors.fg,
            textAlign = TextAlign.Center,
            modifier = Modifier
                .padding(top = Waddle.space.md)
                .semantics { heading() },
        )
        Text(
            text = body,
            style = Waddle.type.body,
            color = Waddle.colors.fgMuted,
            textAlign = TextAlign.Center,
        )
    }
}

/**
 * The penguin in a speech bubble. Its ink is the night ground, so at
 * night it sits on a soft card-coloured halo that gives the bubble an
 * edge; in daylight it stands on the ground as drawn.
 */
@Composable
private fun Mascot() {
    val colors = Waddle.colors
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .size(MASCOT_HALO)
            .background(if (colors.isDark) colors.surface2 else colors.actionSubtle, CircleShape),
    ) {
        Image(
            painter = painterResource(R.drawable.waddle_mark),
            contentDescription = null,
            modifier = Modifier.size(MASCOT),
        )
    }
}

/**
 * A loading placeholder bar on the neutral fill (web `.chat-skeleton`:
 * radius, opacity pulse 1 → .55 over 1.8s, still under reduced motion).
 * The shared `SkeletonBlock` fills with `surface.2`, which in daylight is
 * the ground itself; this uses the neutral badge tone, visible in both
 * themes and on cards.
 */
@Composable
internal fun PlaceholderBar(modifier: Modifier = Modifier, radius: Dp = Waddle.radius.tag) {
    val pulse = if (reducedMotion) {
        null
    } else {
        rememberInfiniteTransition(label = "placeholder").animateFloat(
            initialValue = 1f,
            targetValue = PLACEHOLDER_DIM,
            animationSpec = infiniteRepeatable(
                animation = tween(PLACEHOLDER_CYCLE_MILLIS / 2, easing = HuddleMotion.easing),
                repeatMode = RepeatMode.Reverse,
            ),
            label = "placeholder",
        )
    }
    Box(
        modifier = modifier
            // Read in the layer: the pulse redraws, never recomposes.
            .graphicsLayer { alpha = pulse?.value ?: 1f }
            .background(Waddle.colors.neutralBadge, RoundedCornerShape(radius)),
    )
}

private const val PLACEHOLDER_DIM = 0.55f
private const val PLACEHOLDER_CYCLE_MILLIS = 1800
private val HANDLE_WIDTH = 40.dp
private val HANDLE_HEIGHT = 4.dp
private val MASCOT = 72.dp
private val MASCOT_HALO = 112.dp
private val EMPTY_MAX_WIDTH = 360.dp
