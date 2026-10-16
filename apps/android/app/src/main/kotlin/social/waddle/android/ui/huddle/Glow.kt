package social.waddle.android.ui.huddle

import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Paint
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * A soft halo behind a live thing (web `box-shadow: 0 0 28px -10px
 * glow.live`). Only live things may glow: ember around a huddle, a
 * speaker, a mention. Drawn as a blurred shadow of the element's own
 * rounded shape, so it costs one draw and no offscreen layer; the content
 * draws over it. [spread] shrinks (negative) or grows the shape the glow
 * is cast from, as in CSS.
 */
fun Modifier.glow(
    color: Color,
    blur: Dp,
    cornerRadius: Dp,
    spread: Dp = 0.dp,
    enabled: Boolean = true,
): Modifier = if (!enabled || color.alpha == 0f) {
    this
} else {
    drawBehind {
        val blurPx = blur.toPx()
        val spreadPx = spread.toPx()
        val radiusPx = (cornerRadius.toPx() + spreadPx).coerceAtLeast(0f)
        drawIntoCanvas { canvas ->
            val paint = Paint()
            paint.asFrameworkPaint().apply {
                isAntiAlias = true
                this.color = Color.Transparent.toArgb()
                setShadowLayer(blurPx / 2f, 0f, 0f, color.toArgb())
            }
            canvas.drawRoundRect(
                left = -spreadPx,
                top = -spreadPx,
                right = size.width + spreadPx,
                bottom = size.height + spreadPx,
                radiusX = radiusPx,
                radiusY = radiusPx,
                paint = paint,
            )
        }
    }
}

/** The ring around an avatar: drawn outside its bounds, no layout cost. */
internal fun Modifier.ring(
    gapColor: Color,
    ringColor: Color,
    gap: Dp = 2.dp,
    width: Dp = 2.dp,
    glow: Color? = null,
    glowBlur: Dp = 14.dp,
): Modifier = drawBehind {
    val gapPx = gap.toPx()
    val widthPx = width.toPx()
    val radius = size.minDimension / 2f
    val center = Offset(size.width / 2f, size.height / 2f)
    if (glow != null) {
        drawIntoCanvas { canvas ->
            val paint = Paint()
            paint.asFrameworkPaint().apply {
                isAntiAlias = true
                color = ringColor.toArgb()
                setShadowLayer(glowBlur.toPx() / 2f, 0f, 0f, glow.toArgb())
            }
            canvas.drawCircle(center, radius + gapPx + widthPx, paint)
        }
    }
    drawCircle(ringColor, radius = radius + gapPx + widthPx, center = center)
    drawCircle(gapColor, radius = radius + gapPx, center = center)
}
