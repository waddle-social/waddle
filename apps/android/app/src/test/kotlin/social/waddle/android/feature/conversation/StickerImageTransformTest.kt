package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class StickerImageTransformTest {
    @Test
    fun `images inside the box keep their size`() {
        assertEquals(
            StickerTransform(sampleSize = 1, width = 300, height = 200),
            stickerTransformFor(300, 200),
        )
        assertEquals(
            StickerTransform(sampleSize = 1, width = 512, height = 512),
            stickerTransformFor(512, 512),
        )
    }

    @Test
    fun `oversized images downscale onto the box preserving aspect`() {
        assertEquals(
            // 2048/4 = 512 is still ≥ the target, so the decode can
            // pre-shrink all the way to it.
            StickerTransform(sampleSize = 4, width = 512, height = 256),
            stickerTransformFor(2048, 1024),
        )
        assertEquals(
            StickerTransform(sampleSize = 1, width = 256, height = 512),
            stickerTransformFor(400, 800),
        )
    }

    @Test
    fun `sample size is the largest power of two that keeps the decode above target`() {
        // 4096/8 = 512 ≥ 512, 4096/16 = 256 < 512 → 8.
        assertEquals(8, stickerTransformFor(4096, 4096)?.sampleSize)
        // 1023/2 = 511 < 512 → 1.
        assertEquals(1, stickerTransformFor(1023, 100)?.sampleSize)
        assertEquals(2, stickerTransformFor(1024, 100)?.sampleSize)
    }

    @Test
    fun `extreme aspect ratios never collapse an edge to zero`() {
        val banner = checkNotNull(stickerTransformFor(5000, 1))
        assertEquals(512, banner.width)
        assertEquals(1, banner.height)
    }

    @Test
    fun `pre-shrunk decode always stays at or above the target box`() {
        // The processor hands sampleSize to ImageDecoder and scales the
        // sampled bitmap onto width×height: the sampled longest edge
        // must never land below the target, or the final pass would
        // upscale.
        listOf(513, 1024, 2048, 4095, 10_000).forEach { longest ->
            val plan = checkNotNull(stickerTransformFor(longest, longest / 2))
            assertTrue(
                "sampled $longest/${plan.sampleSize} fell below the box",
                longest / plan.sampleSize >= STICKER_MAX_DIMENSION,
            )
            assertTrue(plan.width <= STICKER_MAX_DIMENSION && plan.height <= STICKER_MAX_DIMENSION)
        }
    }

    @Test
    fun `oriented portrait sources plan portrait output`() {
        // ImageDecoder reports the EXIF-rotated size; a portrait JPEG
        // (even one stored landscape with a 90° orientation tag) plans
        // portrait dimensions.
        assertEquals(
            StickerTransform(sampleSize = 4, width = 256, height = 512),
            stickerTransformFor(1024, 2048),
        )
    }

    @Test
    fun `degenerate sources are rejected`() {
        assertNull(stickerTransformFor(0, 100))
        assertNull(stickerTransformFor(100, -1))
        assertNull(stickerTransformFor(100, 100, maxDimension = 0))
    }
}
