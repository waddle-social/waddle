package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Test

class ConsistentColorHuesTest {

    @Test
    fun `hue is memoized per input`() {
        var calls = 0
        val hues = ConsistentColorHues { input ->
            calls += 1
            input.length.toDouble()
        }
        assertEquals(5.0, hues.hue("Romeo"), 0.0)
        assertEquals(5.0, hues.hue("Romeo"), 0.0)
        assertEquals(1, calls)
        assertEquals(18.0, hues.hue("juliet@capulet.lit"), 0.0)
        assertEquals(2, calls)
    }
}
