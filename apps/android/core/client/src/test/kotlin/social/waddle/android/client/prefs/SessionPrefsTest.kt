package social.waddle.android.client.prefs

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import social.waddle.android.client.MentionRef
import social.waddle.android.client.StickerHash
import java.io.File

/** Plain-JVM DataStore over a temp dir — no Robolectric needed. */
class SessionPrefsTest {
    @get:Rule
    val tempFolder = TemporaryFolder()

    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    private fun newPrefs(): SessionPrefs {
        val file = File(tempFolder.root, "session.preferences_pb")
        return SessionPrefs(PreferenceDataStoreFactory.create(scope = scope) { file })
    }

    @After
    fun tearDown() {
        scope.cancel()
    }

    @Test
    fun `round trips scalar session fields`() = runBlocking {
        val prefs = newPrefs()

        prefs.setServerUrl("https://waddle.test")
        prefs.setSessionId("sess-1")
        prefs.setJoinedRooms(setOf("general@muc.waddle.test"))
        prefs.setLastSeen("alice@waddle.test", "2026-07-15T10:00:00Z")

        assertEquals("https://waddle.test", prefs.serverUrl.first())
        assertEquals("sess-1", prefs.sessionId.first())
        assertEquals(setOf("general@muc.waddle.test"), prefs.joinedRooms.first())
        assertEquals(mapOf("alice@waddle.test" to "2026-07-15T10:00:00Z"), prefs.lastSeen.first())
    }

    @Test
    fun `round trips the resume snapshot and null clears it`() = runBlocking {
        val prefs = newPrefs()
        val snapshot = SmResumeSnapshot(
            previd = "prev-1",
            inboundH = 5u,
            outboundH = 7u,
            maxResumeSeconds = 300u,
            queuedStanzasXml = listOf("<message/>"),
        )

        prefs.setSmResume(snapshot)
        assertEquals(snapshot, prefs.smResume.first())

        prefs.setSmResume(null)
        assertNull(prefs.smResume.first())
    }

    @Test
    fun `outbound queue updates atomically and clears when empty`() = runBlocking {
        val prefs = newPrefs()
        val message = QueuedOutboundMessage(
            conversationJid = "alice@waddle.test",
            isGroupchat = false,
            body = "hello",
            clientStanzaId = "q-1",
            enqueuedAtMillis = 1_000L,
        )

        prefs.updateOutboundQueue { current -> current + message }
        assertEquals(listOf(message), prefs.outboundQueue.first())

        prefs.updateOutboundQueue { current -> current.filterNot { it.clientStanzaId == "q-1" } }
        assertTrue(prefs.outboundQueue.first().isEmpty())
    }

    @Test
    fun `queued encrypted attachments survive the json round trip`() = runBlocking {
        val prefs = newPrefs()
        val message = QueuedOutboundMessage(
            conversationJid = "alice@waddle.test",
            isGroupchat = false,
            body = "",
            clientStanzaId = "q-enc-1",
            enqueuedAtMillis = 1_000L,
            sharedFiles = listOf(
                SharedFileRef(
                    url = "https://files.waddle.test/report.pdf.enc",
                    name = "report.pdf",
                    mediaType = "application/pdf",
                    sizeBytes = 2048L,
                    hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "cGxhaW4=")),
                    encrypted = EncryptedFileRef(
                        cipher = "urn:xmpp:ciphers:aes-256-gcm-nopadding:0",
                        keyB64 = "a2V5",
                        ivB64 = "aXY=",
                        hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "Y2lwaGVy")),
                        sources = listOf("https://files.waddle.test/report.pdf.enc"),
                    ),
                ),
            ),
        )

        prefs.updateOutboundQueue { current -> current + message }

        assertEquals(listOf(message), prefs.outboundQueue.first())
    }

    @Test
    fun `queued mention refs survive the json round trip`() = runBlocking {
        val prefs = newPrefs()
        val message = QueuedOutboundMessage(
            conversationJid = "general@muc.waddle.test",
            isGroupchat = true,
            body = "hi @bob",
            clientStanzaId = "q-2",
            enqueuedAtMillis = 1_000L,
            mentions = listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 3u, end = 7u)),
        )

        prefs.updateOutboundQueue { current -> current + message }

        assertEquals(listOf(message), prefs.outboundQueue.first())
    }

    @Test
    fun `resume cursors round trip and empty clears the key`() = runBlocking {
        val prefs = newPrefs()
        val cursors = mapOf(
            "alice@waddle.test" to ResumeCursor(stanzaId = "s-1", timestamp = "2026-07-15T10:00:00Z"),
        )

        prefs.setResumeCursors(cursors)
        assertEquals(cursors, prefs.resumeCursors.first())

        prefs.setResumeCursors(emptyMap())
        assertTrue(prefs.resumeCursors.first().isEmpty())
    }

    @Test
    fun `resource suffix is eight hex chars and stable across clear`() = runBlocking {
        val prefs = newPrefs()

        val suffix = prefs.resourceSuffix()
        assertTrue("expected 8 hex chars, got $suffix", suffix.matches(Regex("[0-9a-f]{8}")))
        assertEquals(suffix, prefs.resourceSuffix())

        prefs.setSessionId("sess-1")
        prefs.clear()

        assertNull(prefs.sessionId.first())
        assertEquals("per-install suffix survives logout", suffix, prefs.resourceSuffix())
    }
}
