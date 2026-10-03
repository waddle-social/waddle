package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddlePresenceHat

class AuthorBadgesTest {

    @Test
    fun `owner outranks admin outranks moderator`() {
        assertEquals("OWNER", authorityBadge(WaddleMucAffiliation.OWNER, WaddleMucRole.MODERATOR)?.label)
        assertEquals("ADMIN", authorityBadge(WaddleMucAffiliation.ADMIN, WaddleMucRole.MODERATOR)?.label)
        assertEquals("MOD", authorityBadge(WaddleMucAffiliation.MEMBER, WaddleMucRole.MODERATOR)?.label)
        assertNull(authorityBadge(WaddleMucAffiliation.MEMBER, WaddleMucRole.PARTICIPANT))
        assertNull(authorityBadge(null, null))
    }

    @Test
    fun `verified hat outranks bot hat`() {
        val badge = descriptiveBadge(
            listOf(
                WaddlePresenceHat(uri = HAT_URI_BOT, title = "Bot"),
                WaddlePresenceHat(uri = HAT_URI_VERIFIED, title = "Verified"),
            ),
        )
        assertEquals("VERIFIED", badge?.label)
        assertEquals(AuthorBadgeKind.VERIFIED, badge?.kind)
    }

    @Test
    fun `unknown hats fall back to their server title first wins on ties`() {
        val badge = descriptiveBadge(
            listOf(
                WaddlePresenceHat(uri = "urn:example:speaker", title = "Speaker"),
                WaddlePresenceHat(uri = "urn:example:guest", title = "Guest"),
            ),
        )
        assertEquals("Speaker", badge?.label)
        assertEquals(AuthorBadgeKind.HAT, badge?.kind)
    }

    @Test
    fun `authority wins over descriptive hats`() {
        val presence = testPresence(
            from = "room@muc.waddle.test/alice",
            mucAffiliation = WaddleMucAffiliation.OWNER,
            hats = listOf(WaddlePresenceHat(uri = HAT_URI_VERIFIED, title = "Verified")),
        )
        assertEquals("OWNER", authorBadgeOf(presence)?.label)
    }

    @Test
    fun `hats show when no authority applies`() {
        val presence = testPresence(
            from = "room@muc.waddle.test/bot",
            hats = listOf(WaddlePresenceHat(uri = HAT_URI_BOT, title = "Bot")),
        )
        val badge = authorBadgeOf(presence)
        assertEquals("BOT", badge?.label)
        assertEquals(AuthorBadgeKind.BOT, badge?.kind)
    }

    @Test
    fun `bot hat is detected by uri among other hats`() {
        assertTrue(
            hasBotHat(
                listOf(
                    WaddlePresenceHat(uri = HAT_URI_VERIFIED, title = "Verified"),
                    WaddlePresenceHat(uri = HAT_URI_BOT, title = "Helper"),
                ),
            ),
        )
        // A look-alike title is not the hat.
        assertFalse(hasBotHat(listOf(WaddlePresenceHat(uri = "urn:example:bot", title = "Bot"))))
        assertFalse(hasBotHat(emptyList()))
    }

    @Test
    fun `extension bot address is a localpart on extensions of the own domain`() {
        val own = "icepuma@waddle.test/phone"
        assertTrue(isExtensionBotJid("alpha@extensions.waddle.test", own))
        assertTrue(isExtensionBotJid("Alpha@Extensions.WADDLE.test/bot", own))
        assertTrue(isExtensionBotJid("alpha@extensions.waddle.test", "Icepuma@Waddle.Test"))
        // The service itself (no localpart), other domains, and people are not bots.
        assertFalse(isExtensionBotJid("extensions.waddle.test", own))
        assertFalse(isExtensionBotJid("@extensions.waddle.test", own))
        assertFalse(isExtensionBotJid("alpha@extensions.other.test", own))
        assertFalse(isExtensionBotJid("alpha@sub.extensions.waddle.test", own))
        assertFalse(isExtensionBotJid("extensions@waddle.test", own))
        // No account yet: nothing to compare against.
        assertFalse(isExtensionBotJid("alpha@extensions.waddle.test", null))
        assertFalse(isExtensionBotJid("alpha@extensions.waddle.test", "no-domain"))
    }

    @Test
    fun `no presence yields no badge`() {
        assertNull(authorBadgeOf(null))
    }
}
