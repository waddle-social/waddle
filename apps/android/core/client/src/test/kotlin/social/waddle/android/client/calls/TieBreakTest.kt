package social.waddle.android.client.calls

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * XEP-0353 §Tie Breaking (tie-break-1) collation: RFC 4790 §9.3
 * "i;octet" over the UTF-8 encodings, mirroring the web client's
 * tie-break.ts test surface.
 */
class TieBreakTest {
    @Test
    fun `octet compare orders plain ascii`() {
        assertTrue(compareOctetStrings("abc", "abd") < 0)
        assertTrue(compareOctetStrings("abd", "abc") > 0)
        assertTrue(compareOctetStrings("abc", "abc") == 0)
    }

    @Test
    fun `octet compare puts the shorter shared prefix first`() {
        assertTrue(compareOctetStrings("ab", "abc") < 0)
        assertTrue(compareOctetStrings("abc", "ab") > 0)
    }

    @Test
    fun `octet compare is unsigned over multi-byte utf-8`() {
        // 'é' encodes as 0xC3 0xA9; a signed byte compare would sort it
        // BEFORE 'z' (0x7A) because 0xC3 is negative as a Java byte.
        assertTrue(compareOctetStrings("z", "é") < 0)
        assertTrue(compareOctetStrings("é", "z") > 0)
    }

    @Test
    fun `digits sort before lowercase letters as in i-octet`() {
        assertTrue(compareOctetStrings("c1", "ca") < 0)
    }

    @Test
    fun `same bare jid comparison ignores resource and case`() {
        assertTrue(isSameCallBareJid("Bob@Waddle.Test/phone", "bob@waddle.test/desk"))
        assertTrue(isSameCallBareJid("bob@waddle.test", "bob@waddle.test/desk"))
        assertFalse(isSameCallBareJid("bob@waddle.test", "eve@waddle.test"))
    }

    @Test
    fun `lower incoming sid wins the tie-break`() {
        assertTrue(
            incomingProposeWinsTieBreak(
                "c1aaa",
                "c1bbb",
                "bob@waddle.test/phone",
                "alice@waddle.test/desk",
            ),
        )
    }

    @Test
    fun `higher incoming sid loses the tie-break`() {
        assertFalse(
            incomingProposeWinsTieBreak(
                "c1bbb",
                "c1aaa",
                "bob@waddle.test/phone",
                "alice@waddle.test/desk",
            ),
        )
    }

    @Test
    fun `equal sids fall back to the jid comparison`() {
        // XEP-0353 tie-break-1: on identical session ids the action
        // sent by the LOWER of the JabberIDs overrules the other.
        assertTrue(
            incomingProposeWinsTieBreak(
                "c1",
                "c1",
                "alice@waddle.test/phone",
                "bob@waddle.test/desk",
            ),
        )
        assertFalse(
            incomingProposeWinsTieBreak(
                "c1",
                "c1",
                "bob@waddle.test/desk",
                "alice@waddle.test/phone",
            ),
        )
    }
}
