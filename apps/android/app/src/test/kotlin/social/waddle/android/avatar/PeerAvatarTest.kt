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
}
