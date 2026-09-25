package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScore
import social.waddle.client.ffi.WaddleSafetyScores
import social.waddle.client.ffi.WaddleSafetyScoresAction
import social.waddle.client.ffi.WaddleSafetyScoresFastening

/**
 * XEP-0422 `urn:waddle:safety-scores:1` fastenings, fed to the store as
 * the typed FFI records the Rust parser produces (issue #1831).
 */
class SafetyScoresMutationTest {
    private val store = TimelineStore()

    @Before
    fun setUp() {
        store.setOwnBareJid("me@waddle.test")
    }

    private fun mucTimeline() = store.timeline(ROOM).value

    private fun scores(modelVersion: String, harassment: Double = 0.02) = WaddleSafetyScores(
        modelVersion = modelVersion,
        scores = listOf(
            WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 0.92, "is-question-v1"),
            WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, harassment, "safety-harassment-v1"),
        ),
    )

    private fun fastening(scores: WaddleSafetyScores, target: String = "s1") =
        WaddleSafetyScoresFastening(targetId = target, action = WaddleSafetyScoresAction.Apply(scores))

    private fun clearedFastening(target: String = "s1") =
        WaddleSafetyScoresFastening(targetId = target, action = WaddleSafetyScoresAction.Clear)

    private fun liveTarget(stanzaId: String = "s1"): WaddleMessage = testMessage(
        id = "orig-$stanzaId",
        stanzaId = stanzaId,
        from = "$ROOM/alice",
        to = null,
        messageType = "groupchat",
        isMuc = true,
        body = "is anyone around?",
    )

    private fun liveScores(
        scores: WaddleSafetyScores,
        from: String = ROOM,
        target: String = "s1",
    ): WaddleMessage = testMessage(
        id = "scores-${scores.hashCode()}",
        from = from,
        to = null,
        messageType = "groupchat",
        isMuc = true,
        body = null,
        safetyScores = fastening(scores, target),
    )

    private fun liveCleared(from: String = ROOM, target: String = "s1"): WaddleMessage = testMessage(
        id = "cleared-$target-$from",
        from = from,
        to = null,
        messageType = "groupchat",
        isMuc = true,
        body = null,
        safetyScores = clearedFastening(target),
    )

    @Test
    fun `room fastening attaches scores to the target without inserting a row`() {
        store.onLiveMessage(liveTarget())
        val message = liveScores(scores("jev-1"))

        assertTrue("a fastening is a timeline mutation", message.isTimelineMutation())
        assertFalse("a fastening is not a new row", store.onLiveMessage(message))
        val item = mucTimeline().single()
        assertEquals(scores("jev-1"), item.safetyScores)
    }

    @Test
    fun `scores claimed by an occupant are a spoof and are dropped`() {
        store.onLiveMessage(liveTarget())
        store.onLiveMessage(liveScores(scores("jev-1"), from = "$ROOM/mallory"))

        val item = mucTimeline().single()
        assertNull(item.safetyScores)
    }

    @Test
    fun `direct-message fastenings are not applied`() {
        store.onLiveMessage(testMessage(stanzaId = "s1", body = "hi"))
        val inserted = store.onLiveMessage(
            testMessage(id = "f1", body = null, safetyScores = fastening(scores("jev-1"))),
        )

        assertFalse(inserted)
        val item = store.timeline("alice@waddle.test").value.single()
        assertNull(item.safetyScores)
    }

    @Test
    fun `a fastening paged in before its target applies when the target arrives`() {
        // Backwards MAM paging is newest-first: the fastening loads first.
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m2",
                id = "f1",
                from = ROOM,
                to = null,
                messageType = "groupchat",
                body = null,
                timestamp = "2026-09-25T10:01:00Z",
                safetyScores = fastening(scores("jev-1")),
            ),
        )
        assertTrue(mucTimeline().isEmpty())
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                stanzaId = "s1",
                from = "$ROOM/alice",
                to = null,
                messageType = "groupchat",
                body = "is anyone around?",
                timestamp = "2026-09-25T10:00:00Z",
            ),
        )

        assertEquals(scores("jev-1"), mucTimeline().single().safetyScores)
    }

    @Test
    fun `a newer fastening replaces the scores and an older replay cannot revert them`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                stanzaId = "s1",
                from = "$ROOM/alice",
                to = null,
                messageType = "groupchat",
                timestamp = "2026-09-25T10:00:00Z",
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m3",
                id = "f2",
                from = ROOM,
                to = null,
                messageType = "groupchat",
                body = null,
                timestamp = "2026-09-25T10:05:00Z",
                safetyScores = fastening(scores("jev-2", harassment = 0.4)),
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m2",
                id = "f1",
                from = ROOM,
                to = null,
                messageType = "groupchat",
                body = null,
                timestamp = "2026-09-25T10:01:00Z",
                safetyScores = fastening(scores("jev-1")),
            ),
        )

        assertEquals(scores("jev-2", harassment = 0.4), mucTimeline().single().safetyScores)
    }

    @Test
    fun `a clear removes the scores and an older replay cannot resurrect them`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                stanzaId = "s1",
                from = "$ROOM/alice",
                to = null,
                messageType = "groupchat",
                timestamp = "2026-09-25T10:00:00Z",
            ),
        )
        store.onLiveMessage(liveScores(scores("jev-1")))
        assertEquals(scores("jev-1"), mucTimeline().single().safetyScores)

        store.onLiveMessage(liveCleared())
        assertNull(mucTimeline().single().safetyScores)

        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m2",
                id = "f1",
                from = ROOM,
                to = null,
                messageType = "groupchat",
                body = null,
                timestamp = "2026-09-25T09:59:00Z",
                safetyScores = fastening(scores("jev-0")),
            ),
        )
        assertNull(mucTimeline().single().safetyScores)
    }

    @Test
    fun `a live clear survives a re-page of the fastening it anchored to`() {
        val archivedScores = testArchivedMessage(
            mamId = "m2",
            id = "f1",
            from = ROOM,
            to = null,
            messageType = "groupchat",
            body = null,
            timestamp = "2026-09-25T10:01:00Z",
            safetyScores = fastening(scores("jev-1")),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                stanzaId = "s1",
                from = "$ROOM/alice",
                to = null,
                messageType = "groupchat",
                timestamp = "2026-09-25T10:00:00Z",
            ),
        )
        // The fastening is the newest wire stamp seen: the live clear
        // anchors exactly at its instant.
        store.onArchivedMessage(archivedScores)
        store.onLiveMessage(liveCleared())
        // Re-opening the room re-pages the same newest MAM page.
        store.onArchivedMessage(archivedScores)

        assertNull(mucTimeline().single().safetyScores)
    }

    @Test
    fun `a distinct fastening stamped at the live anchor still applies`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                stanzaId = "s1",
                from = "$ROOM/alice",
                to = null,
                messageType = "groupchat",
                timestamp = "2026-09-25T10:00:00Z",
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m2",
                id = "f1",
                from = ROOM,
                to = null,
                messageType = "groupchat",
                body = null,
                timestamp = "2026-09-25T10:01:00Z",
                safetyScores = fastening(scores("jev-1")),
            ),
        )
        store.onLiveMessage(liveCleared())
        // A different re-judgment the archive stamped in the same second.
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m3",
                id = "f2",
                from = ROOM,
                to = null,
                messageType = "groupchat",
                body = null,
                timestamp = "2026-09-25T10:01:00Z",
                safetyScores = fastening(scores("jev-2")),
            ),
        )

        assertEquals(scores("jev-2"), mucTimeline().single().safetyScores)
    }

    @Test
    fun `scores survive the live record superseding its archived twin`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m1",
                id = "orig-s1",
                stanzaId = "s1",
                from = "$ROOM/alice",
                to = null,
                messageType = "groupchat",
                timestamp = "2026-09-25T10:00:00Z",
            ),
        )
        store.onLiveMessage(liveScores(scores("jev-1")))
        store.onLiveMessage(liveTarget())

        assertEquals(scores("jev-1"), mucTimeline().single().safetyScores)
    }

    @Test
    fun `scores for another message leave the row untouched`() {
        store.onLiveMessage(liveTarget())
        store.onLiveMessage(liveScores(scores("jev-1"), target = "s-other"))

        assertNull(mucTimeline().single().safetyScores)
    }

    private companion object {
        const val ROOM = "room@muc.waddle.test"
    }
}
