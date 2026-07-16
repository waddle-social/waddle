package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
    fun `no presence yields no badge`() {
        assertNull(authorBadgeOf(null))
    }
}
