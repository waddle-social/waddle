package social.waddle.android.feature.conversation

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.geometry.isSpecified

internal const val VIEWER_MIN_SCALE = 1f
internal const val VIEWER_MAX_SCALE = 5f
internal const val VIEWER_DOUBLE_TAP_SCALE = 2.5f

/**
 * Pan/zoom state of the full-screen image viewer: a uniform [scale] about the
 * viewport centre plus a screen-space [offset]. Pure so the gesture math is
 * unit-testable without a Compose runtime.
 */
internal data class ViewerTransform(val scale: Float = VIEWER_MIN_SCALE, val offset: Offset = Offset.Zero) {
    val isZoomed: Boolean get() = scale > VIEWER_MIN_SCALE

    /** Apply one pinch/pan step; [centroid] is the gesture centre in viewport coordinates. */
    fun transformed(viewport: Size, centroid: Offset, pan: Offset, zoom: Float): ViewerTransform {
        // The gesture helpers return Offset.Unspecified (NaN) for the centroid on the
        // event where the last finger lifts; one NaN would poison the layer's
        // translation and the image would vanish.
        if (!centroid.isSpecified || !pan.isSpecified) return this
        if (!zoom.isFinite() || zoom <= 0f) return this
        val newScale = (scale * zoom).coerceIn(VIEWER_MIN_SCALE, VIEWER_MAX_SCALE)
        val applied = newScale / scale
        val centre = Offset(viewport.width / 2f, viewport.height / 2f)
        // Keeps the content point under the centroid fixed while scaling.
        val moved = (centroid - centre) * (1f - applied) + offset * applied + pan
        return ViewerTransform(newScale, moved).clamped(viewport)
    }

    /** Double-tap: zoom in about [tap], or back to fit when already zoomed. */
    fun toggled(viewport: Size, tap: Offset): ViewerTransform {
        if (isZoomed) return ViewerTransform()
        val centre = Offset(viewport.width / 2f, viewport.height / 2f)
        return ViewerTransform(VIEWER_DOUBLE_TAP_SCALE, (centre - tap) * (VIEWER_DOUBLE_TAP_SCALE - 1f))
            .clamped(viewport)
    }

    /** Keep the scaled content covering the viewport: no panning past its edges. */
    fun clamped(viewport: Size): ViewerTransform {
        if (scale <= VIEWER_MIN_SCALE) return ViewerTransform()
        val maxX = viewport.width * (scale - 1f) / 2f
        val maxY = viewport.height * (scale - 1f) / 2f
        return copy(offset = Offset(offset.x.coerceIn(-maxX, maxX), offset.y.coerceIn(-maxY, maxY)))
    }
}
