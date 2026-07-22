package social.waddle.android.feature.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AvatarTransformTest {
    @Test
    fun `a landscape source center-crops to its height`() {
        val geometry = avatarGeometry(width = 1000, height = 400)
        assertEquals(AvatarGeometry(cropX = 300, cropY = 0, cropSize = 400, outputSize = 400), geometry)
    }

    @Test
    fun `a portrait source center-crops to its width`() {
        val geometry = avatarGeometry(width = 400, height = 1000)
        assertEquals(AvatarGeometry(cropX = 0, cropY = 300, cropSize = 400, outputSize = 400), geometry)
    }

    @Test
    fun `a large square source downscales to the 512 cap`() {
        val geometry = avatarGeometry(width = 2048, height = 2048)
        assertEquals(AvatarGeometry(cropX = 0, cropY = 0, cropSize = 2048, outputSize = 512), geometry)
    }

    @Test
    fun `a large rectangle crops first, then downscales`() {
        val geometry = avatarGeometry(width = 4000, height = 1000)
        assertEquals(AvatarGeometry(cropX = 1500, cropY = 0, cropSize = 1000, outputSize = 512), geometry)
    }

    @Test
    fun `a small source is never upscaled`() {
        val geometry = avatarGeometry(width = 100, height = 80)
        assertEquals(AvatarGeometry(cropX = 10, cropY = 0, cropSize = 80, outputSize = 80), geometry)
    }

    @Test
    fun `an exactly-512 square passes through untouched`() {
        val geometry = avatarGeometry(width = 512, height = 512)
        assertEquals(AvatarGeometry(cropX = 0, cropY = 0, cropSize = 512, outputSize = 512), geometry)
    }

    @Test
    fun `degenerate inputs yield no geometry`() {
        assertNull(avatarGeometry(width = 0, height = 100))
        assertNull(avatarGeometry(width = 100, height = 0))
        assertNull(avatarGeometry(width = -1, height = -1))
        assertNull(avatarGeometry(width = 100, height = 100, maxDimension = 0))
    }
}
