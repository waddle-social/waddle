package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddlePresenceHat
import social.waddle.client.ffi.WaddleRoomBot

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
    fun `declared bots are the room bot list plus hat-learned jids, normalized`() {
        val declared = declaredBotJidsOf(
            roomBots = listOf(WaddleRoomBot(jid = "Alpha@Extensions.Waddle.Test", name = "Alpha")),
            hatLearned = setOf("zeta@extensions.waddle.test", "alpha@extensions.waddle.test"),
        )
        assertEquals(setOf("alpha@extensions.waddle.test", "zeta@extensions.waddle.test"), declared)
        assertTrue(declaredBotJidsOf(emptyList(), emptySet()).isEmpty())
    }

    @Test
    fun `a declared bot author gets the bot badge without a nick lookup`() {
        val declared = setOf("alpha@extensions.waddle.test")
        var lookedUp = false
        val badge = messageAuthorBadgeOf("Alpha@Extensions.Waddle.Test", declared) {
            lookedUp = true
            // A person reusing the bot's nick must not change the badge.
            testPresence(mucAffiliation = WaddleMucAffiliation.OWNER)
        }
        assertEquals(AuthorBadgeKind.BOT, badge?.kind)
        assertEquals("BOT", badge?.label)
        assertFalse(lookedUp)
    }

    @Test
    fun `other authors keep hat and authority badges from the nick presence`() {
        val declared = setOf("alpha@extensions.waddle.test")
        val owner = testPresence(mucAffiliation = WaddleMucAffiliation.OWNER)
        assertEquals("OWNER", messageAuthorBadgeOf("bob@waddle.test", declared) { owner }?.label)
        // No pinned author JID yet (e.g. archive row): nick presence decides.
        assertEquals("OWNER", messageAuthorBadgeOf(null, declared) { owner }?.label)
        assertNull(messageAuthorBadgeOf("bob@waddle.test", declared) { null })
        // A bot-looking address alone is not a declaration.
        assertNull(messageAuthorBadgeOf("alpha@extensions.waddle.test", emptySet()) { null })
    }

    @Test
    fun `no presence yields no badge`() {
        assertNull(authorBadgeOf(null))
    }
}
