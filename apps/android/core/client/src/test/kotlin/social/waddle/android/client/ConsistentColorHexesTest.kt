package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Test

class ConsistentColorHexesTest {

    @Test
    fun `hex is memoized per input and lightness`() {
        var calls = 0
        val hexes = ConsistentColorHexes { input, _, lightness ->
            calls += 1
            "#%02x0000".format(input.length + lightness.toInt())
        }
        assertEquals("#370000", hexes.hex("Romeo", 100.0, 50.0))
        assertEquals("#370000", hexes.hex("Romeo", 100.0, 50.0))
        assertEquals(1, calls)
        assertEquals("#320000", hexes.hex("Romeo", 100.0, 45.0))
        assertEquals(2, calls)
    }
}
