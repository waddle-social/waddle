package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.MentionRef
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.prefs.SharedFileRef
import social.waddle.android.client.store.MessageTombstone
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleMessage

class ComposerControllerTest {
    private fun channelController(screenThreadId: String? = null) =
        ComposerController(ROOM_JID, isGroupchat = true, screenThreadId = screenThreadId)

    private fun dmController(screenThreadId: String? = null) =
        ComposerController(PEER_JID, isGroupchat = false, screenThreadId = screenThreadId)

    private fun itemOf(
        message: WaddleMessage,
        isMine: Boolean = false,
        tombstone: MessageTombstone? = null,
    ): TimelineItem = TimelineItem(
        id = message.stanzaId ?: message.originId ?: checkNotNull(message.id),
        conversationJid = ROOM_JID,
        from = message.from,
        body = message.body.orEmpty(),
        timestamp = message.timestamp,
        isMine = isMine,
        source = TimelineSource.Live(message),
        tombstone = tombstone,
    )

    @Test
    fun `channel reply targets the room-assigned stanza id`() {
        val controller = channelController()
        val item = itemOf(
            testMessage(
                stanzaId = "room-id",
                stanzaIdBy = ROOM_JID,
                from = "$ROOM_JID/alice",
                body = "parent body",
            ),
        )

        controller.startReply(item)

        val mode = controller.mode.value as ComposerMode.Replying
        assertEquals("room-id", mode.targetId)
        assertEquals("$ROOM_JID/alice", mode.authorJid)
        assertEquals("alice", mode.authorName)
        assertEquals("parent body", mode.previewBody)
        // Channels only thread explicit replies.
        assertNull(mode.threadId)
    }

    @Test
    fun `channel rows without a room-assigned id cannot be replied to`() {
        val controller = channelController()
        val item = itemOf(testMessage(stanzaId = "occupant-injected", from = "$ROOM_JID/alice"))

        controller.startReply(item)

        assertEquals(ComposerMode.Normal, controller.mode.value)
    }

    @Test
    fun `dm reply roots an implicit thread at the parent`() {
        val controller = dmController()
        val item = itemOf(
            testMessage(id = "msg-1", originId = "origin-1", from = "$PEER_JID/phone"),
        )

        controller.startReply(item)

        val mode = controller.mode.value as ComposerMode.Replying
        assertEquals("origin-1", mode.targetId)
        assertEquals(PEER_JID, mode.authorJid)
        assertEquals("alice", mode.authorName)
        assertEquals("the implicit thread roots at the parent", "origin-1", mode.threadId)
    }

    @Test
    fun `a parent's explicit thread is echoed instead of a new root`() {
        val controller = dmController()
        val item = itemOf(testMessage(originId = "origin-1", thread = "t-9"))

        controller.startReply(item)

        assertEquals("t-9", (controller.mode.value as ComposerMode.Replying).threadId)
    }

    @Test
    fun `tombstoned rows can be neither replied to nor edited`() {
        val controller = dmController()
        val item = itemOf(
            testMessage(originId = "origin-1"),
            isMine = true,
            tombstone = MessageTombstone.Retracted,
        )

        controller.startReply(item)
        assertEquals(ComposerMode.Normal, controller.mode.value)

        controller.startEdit(item)
        assertEquals(ComposerMode.Normal, controller.mode.value)
    }

    @Test
    fun `startEdit targets the author-assigned id of an own message`() {
        val controller = dmController()
        val foreign = itemOf(testMessage(originId = "origin-1", body = "not mine"))
        controller.startEdit(foreign)
        assertEquals(ComposerMode.Normal, controller.mode.value)

        val own = itemOf(
            testMessage(originId = "origin-1", body = "typo", thread = "t-1"),
            isMine = true,
        )
        controller.startEdit(own)

        val mode = controller.mode.value as ComposerMode.Editing
        assertEquals("origin-1", mode.targetId)
        assertEquals("typo", mode.originalBody)
        assertEquals("t-1", mode.threadId)
    }

    @Test
    fun `failed correction restores edit mode with the attempted text`() {
        val controller = dmController()
        controller.startEdit(itemOf(testMessage(originId = "origin-1", body = "typo"), isMine = true))
        val mode = controller.mode.value as ComposerMode.Editing
        // The send path returns the composer to Normal before dispatch.
        controller.cancelEdit()

        controller.restoreFailedEdit(mode, attemptedBody = "fixed text")

        val restored = controller.mode.value as ComposerMode.Editing
        assertEquals("origin-1", restored.targetId)
        assertEquals("fixed text", restored.originalBody)
        assertEquals("the composer re-runs its prefill", mode.attempt + 1, restored.attempt)
    }

    @Test
    fun `failed correction never clobbers newer composer intent`() {
        val controller = dmController()
        controller.startEdit(itemOf(testMessage(originId = "origin-1", body = "typo"), isMine = true))
        val mode = controller.mode.value as ComposerMode.Editing
        controller.cancelEdit()

        // The user started a reply while the correction was in flight.
        controller.startReply(itemOf(testMessage(originId = "origin-2")))
        controller.restoreFailedEdit(mode, attemptedBody = "fixed text")

        assertTrue(controller.mode.value is ComposerMode.Replying)
    }

    @Test
    fun `cancelReply leaves an edit in progress untouched`() {
        val controller = dmController()
        controller.startEdit(itemOf(testMessage(originId = "origin-1"), isMine = true))

        controller.cancelReply()

        assertTrue(controller.mode.value is ComposerMode.Editing)
    }

    @Test
    fun `reply extras carry the target and fall back to the screen thread`() {
        val controller = dmController(screenThreadId = "screen-thread")
        val mode = ComposerMode.Replying(
            targetId = "origin-1",
            authorJid = PEER_JID,
            authorName = "alice",
            previewBody = "parent",
            threadId = null,
        )

        val extras = controller.extrasFor(mode)

        assertEquals(
            MessageSendExtras(
                replyToId = "origin-1",
                replyToAuthorJid = PEER_JID,
                replyParentBody = "parent",
                threadId = "screen-thread",
            ),
            extras,
        )
    }

    @Test
    fun `files ride the reply stanza`() {
        val controller = dmController()
        val file = SharedFileRef(url = "https://files.waddle.test/cat.png")
        val mode = ComposerMode.Replying(
            targetId = "origin-1",
            authorJid = PEER_JID,
            authorName = "alice",
            previewBody = "parent",
            threadId = "t-1",
        )

        val extras = controller.extrasFor(mode, files = listOf(file))

        assertEquals("origin-1", extras?.replyToId)
        assertEquals("t-1", extras?.threadId)
        assertEquals(listOf(file), extras?.sharedFiles)
    }

    @Test
    fun `files-only sends still target the screen thread`() {
        val controller = channelController(screenThreadId = "t-1")
        val file = SharedFileRef(url = "https://files.waddle.test/cat.png")

        val extras = controller.extrasFor(ComposerMode.Normal, files = listOf(file))

        assertEquals(MessageSendExtras(threadId = "t-1", sharedFiles = listOf(file)), extras)
    }

    @Test
    fun `plain sends produce thread extras only on a thread screen`() {
        assertEquals(
            MessageSendExtras(threadId = "t-1"),
            channelController(screenThreadId = "t-1").extrasFor(ComposerMode.Normal),
        )
        assertNull(channelController().extrasFor(ComposerMode.Normal))
    }

    @Test
    fun `mentions ride every extras shape including plain sends`() {
        val mentions = listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 0u, end = 4u))
        val reply = ComposerMode.Replying(
            targetId = "origin-1",
            authorJid = PEER_JID,
            authorName = "alice",
            previewBody = "parent",
            threadId = null,
        )

        assertEquals(
            mentions,
            dmController().extrasFor(reply, mentions = mentions)?.mentions,
        )
        assertEquals(
            mentions,
            channelController(screenThreadId = "t-1")
                .extrasFor(ComposerMode.Normal, mentions = mentions)?.mentions,
        )
        // A mention on a plain send is itself enough to need extras.
        assertEquals(
            MessageSendExtras(mentions = mentions),
            channelController().extrasFor(ComposerMode.Normal, mentions = mentions),
        )
    }

    private companion object {
        const val ROOM_JID = "general@muc.waddle.test"
        const val PEER_JID = "alice@waddle.test"
    }
}
