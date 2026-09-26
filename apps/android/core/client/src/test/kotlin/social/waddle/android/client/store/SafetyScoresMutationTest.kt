package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScore
import social.waddle.client.ffi.WaddleSafetyScores
import social.waddle.client.ffi.WaddleSafetyScoresFastening

/** XEP-0422 room-authored result targets one source origin, room stanza and revision. */
class SafetyScoresMutationTest {
    private val store = TimelineStore()
    private val room = "room@muc.waddle.test"

    @Before fun setUp() {
        store.setOwnBareJid("me@waddle.test")
    }

    private fun scores(version: String) = WaddleSafetyScores(
        modelVersion = version,
        scores = listOf(WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 0.92, "q-v1")),
    )

    private fun fastening(
        version: String = "jev-1",
        stanzaId: String = "s1",
        originId: String = "origin-1",
        revisionId: String = "s1",
    ) = WaddleSafetyScoresFastening(
        targetOriginId = originId,
        targetStanzaId = stanzaId,
        targetStanzaBy = room,
        sourceRevisionId = revisionId,
        scores = scores(version),
    )

    private fun source(stanzaId: String = "s1"): WaddleMessage = testMessage(
        id = "origin-1",
        originId = "origin-1",
        stanzaId = stanzaId,
        stanzaIdBy = room,
        from = "$room/alice",
        to = null,
        messageType = "groupchat",
        isMuc = true,
        body = "is anyone around?",
    )

    private fun result(fastening: WaddleSafetyScoresFastening, from: String = room) = testMessage(
        id = "score-${fastening.scores.modelVersion}",
        from = from,
        to = null,
        messageType = "groupchat",
        isMuc = true,
        body = null,
        safetyScores = fastening,
    )

    private fun row() = store.timeline(room).value.single()

    @Test fun roomResultAnnotatesWithoutInserting() {
        store.onLiveMessage(source())
        assertFalse(store.onLiveMessage(result(fastening())))
        assertEquals(1, store.timeline(room).value.size)
        assertEquals(scores("jev-1"), row().safetyScores)
    }

    @Test fun originAndRoomStanzaMustBothMatch() {
        store.onLiveMessage(source())
        store.onLiveMessage(result(fastening(originId = "colliding-origin")))
        assertNull(row().safetyScores)
        store.onLiveMessage(result(fastening(stanzaId = "other")))
        assertNull(row().safetyScores)
        store.onLiveMessage(result(fastening()))
        assertEquals(scores("jev-1"), row().safetyScores)
    }

    @Test fun occupantCannotPublishScores() {
        store.onLiveMessage(source())
        store.onLiveMessage(result(fastening(), from = "$room/mallory"))
        assertNull(row().safetyScores)
    }

    @Test fun archivedResultBeforeSourceIsParked() {
        store.onArchivedMessage(testArchivedMessage(
            mamId = "score-mam",
            from = room,
            to = null,
            messageType = "groupchat",
            body = null,
            safetyScores = fastening(),
        ))
        store.onArchivedMessage(testArchivedMessage(
            mamId = "source-mam",
            id = "origin-1",
            originId = "origin-1",
            stanzaId = "s1",
            stanzaIdBy = room,
            from = "$room/alice",
            to = null,
            messageType = "groupchat",
            body = "is anyone around?",
        ))
        assertEquals(scores("jev-1"), row().safetyScores)
    }

    @Test fun newestFirstArchiveKeepsScoreUntilCorrectionArrives() {
        store.onArchivedMessage(testArchivedMessage(
            mamId = "score-mam", from = room, to = null, messageType = "groupchat", body = null,
            safetyScores = fastening(version = "jev-2", revisionId = "edit-1"),
            timestamp = "2026-09-25T10:02:00Z",
        ))
        store.onArchivedMessage(testArchivedMessage(
            mamId = "edit-mam", id = "edit-origin", stanzaId = "edit-1", stanzaIdBy = room,
            from = "$room/alice", to = null, messageType = "groupchat", body = "edited",
            replacesId = "origin-1", timestamp = "2026-09-25T10:01:00Z",
        ))
        store.onArchivedMessage(testArchivedMessage(
            mamId = "source-mam", id = "origin-1", originId = "origin-1",
            stanzaId = "s1", stanzaIdBy = room, from = "$room/alice", to = null,
            messageType = "groupchat", body = "original", timestamp = "2026-09-25T10:00:00Z",
        ))
        assertEquals("edited", row().body)
        assertEquals(scores("jev-2"), row().safetyScores)
    }

    @Test fun correctionClearsAndOldRevisionCannotReattach() {
        store.onLiveMessage(source())
        store.onLiveMessage(result(fastening()))
        store.onLiveMessage(testMessage(
            id = "edit-origin",
            stanzaId = "edit-1",
            stanzaIdBy = room,
            from = "$room/alice",
            to = null,
            messageType = "groupchat",
            isMuc = true,
            body = "edited",
            replacesId = "origin-1",
        ))
        assertNull(row().safetyScores)
        store.onLiveMessage(result(fastening()))
        assertNull(row().safetyScores)
        store.onLiveMessage(result(fastening(version = "jev-2", revisionId = "edit-1")))
        assertEquals(scores("jev-2"), row().safetyScores)
    }
}
