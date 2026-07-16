package social.waddle.android.service

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testMessage
import social.waddle.android.client.testSessionInfo
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddleStanzaId

class NotificationPolicyTest {
    private val userPrefs = UserPrefs(InMemoryPreferencesDataStore())
    private val session = MutableStateFlow<WaddleSessionInfo?>(testSessionInfo())
    private var appVisible = false

    /** XEP-0492 mode snapshot; unlisted JIDs resolve to `always`. */
    private val notifyModes = mutableMapOf<String, WaddleNotifyMode>()

    private val policy = NotificationPolicy(
        userPrefs = userPrefs,
        currentSession = session,
        isAppVisible = { appVisible },
        notifyModeFor = { jid, _ -> notifyModes[jid] ?: WaddleNotifyMode.ALWAYS },
    )

    @Test
    fun `a peer chat message notifies with the bare conversation jid`() = runTest {
        val decision = policy.decide(
            testMessage(from = "alice@waddle.test/phone", body = "hi there"),
        )

        assertNotNull(decision)
        assertEquals("alice@waddle.test", decision?.conversationJid)
        assertEquals("alice", decision?.sender)
        assertEquals(false, decision?.isGroupchat)
        assertEquals("hi there", decision?.body)
    }

    @Test
    fun `a groupchat message notifies with the nick as sender`() = runTest {
        val decision = policy.decide(
            testMessage(
                from = "general@muc.waddle.test/alice",
                messageType = "groupchat",
                isMuc = true,
            ),
        )

        assertNotNull(decision)
        assertEquals("general@muc.waddle.test", decision?.conversationJid)
        assertEquals("alice", decision?.sender)
        assertEquals(true, decision?.isGroupchat)
    }

    @Test
    fun `bodyless messages never notify`() = runTest {
        assertNull(policy.decide(testMessage(body = null)))
    }

    @Test
    fun `timeline mutations never notify`() = runTest {
        assertNull(policy.decide(testMessage(replacesId = "msg-0", body = "edited")))
        assertNull(policy.decide(testMessage(retractsId = "msg-0")))
        assertNull(policy.decide(testMessage(reactionTargetId = "msg-0", reactionEmojis = listOf("👍"))))
        assertNull(
            policy.decide(
                testMessage(
                    from = "general@muc.waddle.test/mod",
                    messageType = "groupchat",
                    isMuc = true,
                    moderationTargetId = "msg-0",
                ),
            ),
        )
    }

    @Test
    fun `a visible app suppresses notifications`() = runTest {
        appVisible = true
        assertNull(policy.decide(testMessage()))
    }

    @Test
    fun `the notifications pref gates everything`() = runTest {
        userPrefs.setNotificationsEnabled(false)
        assertNull(policy.decide(testMessage()))

        userPrefs.setNotificationsEnabled(true)
        assertNotNull(policy.decide(testMessage()))
    }

    @Test
    fun `no signed-in session means no notification`() = runTest {
        session.value = null
        assertNull(policy.decide(testMessage()))
    }

    @Test
    fun `non-chat non-groupchat types never notify`() = runTest {
        assertNull(policy.decide(testMessage(messageType = "normal")))
        assertNull(policy.decide(testMessage(messageType = "headline")))
    }

    @Test
    fun `senderless messages never notify`() = runTest {
        assertNull(policy.decide(testMessage(from = null)))
    }

    @Test
    fun `own dm carbons never notify`() = runTest {
        assertNull(policy.decide(testMessage(from = "icepuma@waddle.test/phone")))
    }

    @Test
    fun `own muc echoes never notify while other nicks do`() = runTest {
        assertNull(
            policy.decide(
                testMessage(
                    from = "general@muc.waddle.test/icepuma",
                    messageType = "groupchat",
                    isMuc = true,
                ),
            ),
        )
        assertNotNull(
            policy.decide(
                testMessage(
                    from = "general@muc.waddle.test/alice",
                    messageType = "groupchat",
                    isMuc = true,
                ),
            ),
        )
    }

    @Test
    fun `dm displayed target prefers the origin id over the message id`() = runTest {
        val decision = policy.decide(testMessage(id = "msg-1", originId = "origin-1"))

        assertEquals("origin-1", decision?.displayedTarget?.markerId)

        val fallback = policy.decide(testMessage(id = "msg-1", originId = null))
        assertEquals("msg-1", fallback?.displayedTarget?.markerId)
    }

    @Test
    fun `dm without any usable id still notifies without a target`() = runTest {
        val decision = policy.decide(testMessage(id = null, originId = null))

        assertNotNull(decision)
        assertNull(decision?.displayedTarget)
    }

    @Test
    fun `dm mds pair only accepts own-account or own-domain stanza ids`() = runTest {
        val decision = policy.decide(
            testMessage(from = "alice@waddle.test/phone", displayedMarkerRequested = true).copy(
                stanzaIds = listOf(
                    WaddleStanzaId(id = "spoofed", by = "attacker@evil.example"),
                    WaddleStanzaId(id = "sid-1", by = "waddle.test"),
                ),
            ),
        )

        val target = decision?.displayedTarget
        assertNotNull(target)
        assertEquals("sid-1", target?.stanzaId)
        assertEquals("waddle.test", target?.stanzaIdBy)
        assertEquals(true, target?.markerRequested)
    }

    @Test
    fun `muc displayed target requires the room-assigned stanza id`() = runTest {
        val message = testMessage(
            from = "general@muc.waddle.test/alice",
            messageType = "groupchat",
            isMuc = true,
        )

        val without = policy.decide(
            message.copy(stanzaIds = listOf(WaddleStanzaId(id = "spoofed", by = "evil.example"))),
        )
        assertNotNull(without)
        assertNull("sender-controlled stanza ids must not become targets", without?.displayedTarget)

        val with = policy.decide(
            message.copy(stanzaIds = listOf(WaddleStanzaId(id = "room-1", by = "GENERAL@muc.waddle.test"))),
        )
        val target = with?.displayedTarget
        assertEquals("room-1", target?.markerId)
        assertEquals("room-1", target?.stanzaId)
        assertTrue(target?.stanzaIdBy.equals("general@muc.waddle.test", ignoreCase = true))
    }

    @Test
    fun `a self mention marks the decision as a mention`() = runTest {
        val decision = policy.decide(
            testMessage(
                from = "general@muc.waddle.test/alice",
                messageType = "groupchat",
                isMuc = true,
                mentionUris = listOf("xmpp:icepuma@waddle.test"),
            ),
        )

        assertEquals(true, decision?.isMention)
    }

    @Test
    fun `a broadcast mention marks the decision as a mention`() = runTest {
        val decision = policy.decide(
            testMessage(
                from = "general@muc.waddle.test/alice",
                messageType = "groupchat",
                isMuc = true,
                broadcastMention = "xmpp:@everyone",
                mentionUris = listOf("xmpp:@everyone"),
            ),
        )

        assertEquals(true, decision?.isMention)
    }

    @Test
    fun `mentions of other users stay ordinary notifications`() = runTest {
        val decision = policy.decide(
            testMessage(
                from = "general@muc.waddle.test/alice",
                messageType = "groupchat",
                isMuc = true,
                mentionUris = listOf("xmpp:carol@waddle.test"),
            ),
        )

        assertEquals(false, decision?.isMention)
    }

    // ── XEP-0492 per-conversation gating ─────────────────────────────

    private fun mucMessage(
        mentionUris: List<String> = emptyList(),
        broadcastMention: String? = null,
    ) = testMessage(
        from = "general@muc.waddle.test/alice",
        messageType = "groupchat",
        isMuc = true,
        mentionUris = mentionUris,
        broadcastMention = broadcastMention,
    )

    @Test
    fun `a muted room never notifies, mentions included`() = runTest {
        notifyModes["general@muc.waddle.test"] = WaddleNotifyMode.NEVER

        assertNull(policy.decide(mucMessage()))
        assertNull(policy.decide(mucMessage(mentionUris = listOf("xmpp:icepuma@waddle.test"))))
        assertNull(policy.decide(mucMessage(broadcastMention = "xmpp:@everyone")))
    }

    @Test
    fun `a muted dm never notifies`() = runTest {
        notifyModes["alice@waddle.test"] = WaddleNotifyMode.NEVER

        assertNull(policy.decide(testMessage(from = "alice@waddle.test/phone")))
    }

    @Test
    fun `on-mention suppresses plain room traffic`() = runTest {
        notifyModes["general@muc.waddle.test"] = WaddleNotifyMode.ON_MENTION

        assertNull(policy.decide(mucMessage()))
        assertNull(policy.decide(mucMessage(mentionUris = listOf("xmpp:carol@waddle.test"))))
    }

    @Test
    fun `on-mention notifies self and broadcast mentions`() = runTest {
        notifyModes["general@muc.waddle.test"] = WaddleNotifyMode.ON_MENTION

        val selfMention = policy.decide(mucMessage(mentionUris = listOf("xmpp:icepuma@waddle.test")))
        assertEquals(true, selfMention?.isMention)

        val broadcast = policy.decide(mucMessage(broadcastMention = "xmpp:@everyone"))
        assertEquals(true, broadcast?.isMention)
    }

    @Test
    fun `on-mention gates dms on a self mention too`() = runTest {
        notifyModes["alice@waddle.test"] = WaddleNotifyMode.ON_MENTION

        assertNull(policy.decide(testMessage(from = "alice@waddle.test/phone")))
        assertNotNull(
            policy.decide(
                testMessage(
                    from = "alice@waddle.test/phone",
                    mentionUris = listOf("xmpp:icepuma@waddle.test"),
                ),
            ),
        )
    }

    @Test
    fun `always keeps current behavior for plain traffic`() = runTest {
        notifyModes["general@muc.waddle.test"] = WaddleNotifyMode.ALWAYS

        assertNotNull(policy.decide(mucMessage()))
    }
}
