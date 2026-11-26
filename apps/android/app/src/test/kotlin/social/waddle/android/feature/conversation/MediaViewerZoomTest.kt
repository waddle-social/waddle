package social.waddle.android.feature.conversation

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MediaViewerZoomTest {
    private val viewport = Size(1000f, 2000f)
    private val centre = Offset(500f, 1000f)

    @Test
    fun `pinch scales and clamps to the allowed range`() {
        val zoomed = ViewerTransform().transformed(viewport, centre, Offset.Zero, 100f)
        assertEquals(VIEWER_MAX_SCALE, zoomed.scale, 0f)
        val out = zoomed.transformed(viewport, centre, Offset.Zero, 0.001f)
        assertEquals(ViewerTransform(), out)
    }

    @Test
    fun `pinch about the centre does not move the content`() {
        val zoomed = ViewerTransform().transformed(viewport, centre, Offset.Zero, 2f)
        assertEquals(2f, zoomed.scale, 0f)
        assertEquals(Offset.Zero, zoomed.offset)
    }

    @Test
    fun `pinch about a corner keeps that point fixed`() {
        val corner = Offset(1000f, 2000f)
        val zoomed = ViewerTransform().transformed(viewport, corner, Offset.Zero, 2f)
        // Point p under the corner maps back to the corner: centre + s*(p-centre) + t.
        val p = corner
        val screen = centre + (p - centre) * zoomed.scale + zoomed.offset
        assertEquals(corner, screen)
    }

    @Test
    fun `pan is clamped to the scaled content edges`() {
        val zoomed = ViewerTransform(2f).transformed(viewport, centre, Offset(5000f, -5000f), 1f)
        assertEquals(Offset(500f, -1000f), zoomed.offset)
    }

    @Test
    fun `fit scale cannot be panned`() {
        val t = ViewerTransform().transformed(viewport, centre, Offset(50f, 50f), 1f)
        assertEquals(ViewerTransform(), t)
        assertFalse(t.isZoomed)
    }

    @Test
    fun `double tap zooms in about the tap then resets`() {
        val tap = Offset(900f, 1900f)
        val zoomed = ViewerTransform().toggled(viewport, tap)
        assertTrue(zoomed.isZoomed)
        assertEquals(VIEWER_DOUBLE_TAP_SCALE, zoomed.scale, 0f)
        assertEquals(ViewerTransform(), zoomed.toggled(viewport, tap))
    }

    @Test
    fun `lift-off events with an unspecified centroid leave the transform untouched`() {
        val zoomed = ViewerTransform(3f, Offset(120f, -80f))
        assertEquals(zoomed, zoomed.transformed(viewport, Offset.Unspecified, Offset.Zero, 1f))
        assertEquals(zoomed, zoomed.transformed(viewport, centre, Offset.Unspecified, 1f))
        assertEquals(zoomed, zoomed.transformed(viewport, centre, Offset.Zero, Float.NaN))
    }
}
