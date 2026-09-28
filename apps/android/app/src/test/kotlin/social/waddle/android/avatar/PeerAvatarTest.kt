package social.waddle.android.avatar

import org.junit.Assert.assertEquals
import org.junit.Test

/** Web `AppAvatar` initials parity. */
class PeerAvatarTest {
    @Test
    fun initialsTakeTheFirstLetterOfUpToTwoWords() {
        assertEquals("AL", initialsOf("ada lovelace"))
        assertEquals("AB", initialsOf("Ada Byron Lovelace"))
        assertEquals("A", initialsOf("ada"))
        assertEquals("AL", initialsOf("ada  lovelace"))
        assertEquals("", initialsOf(""))
    }

    @Test
    fun initialsKeepWholeCodePoints() {
        assertEquals("🐧P", initialsOf("🐧 penguin"))
    }

    @Test
    fun sampleSizeBoundsTheLongerEdge() {
        assertEquals(1, sampleSizeFor(longerEdge = 256, maxEdge = 256))
        assertEquals(2, sampleSizeFor(longerEdge = 512, maxEdge = 256))
        assertEquals(4, sampleSizeFor(longerEdge = 1024, maxEdge = 256))
        // A 4000x100 banner is bounded by its width, not its short side.
        assertEquals(8, sampleSizeFor(longerEdge = 4000, maxEdge = 256))
    }
}
