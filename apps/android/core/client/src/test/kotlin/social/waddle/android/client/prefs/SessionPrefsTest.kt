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
import social.waddle.android.client.FileDisposition
import social.waddle.android.client.MentionRef
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
    fun `active session and projection fields round trip`() = runBlocking {
        val prefs = newPrefs()

        prefs.activateSession(OWNER_A, "sess-a")
        prefs.setJoinedRooms(setOf("general@muc.waddle.test"))
        prefs.setLastSeen("alice@waddle.test", "2026-07-15T10:00:00Z")

        assertEquals(OWNER_A, prefs.ownerBareJid.first())
        assertEquals("sess-a", prefs.sessionId.first())
        assertEquals(setOf("general@muc.waddle.test"), prefs.joinedRooms.first())
        assertEquals(
            mapOf("alice@waddle.test" to "2026-07-15T10:00:00Z"),
            prefs.lastSeen.first(),
        )
    }

    @Test
    fun `one journal round trip preserves every native phase and exact attempt`() = runBlocking {
        val prefs = newPrefs()
        prefs.activateSession(OWNER_A, "sess-a")
        val attempt = attempt(OWNER_A, "00000000-0000-4000-8000-000000000001", 9u)
        val rows = NativeOutboundPhase.entries.mapIndexed { index, phase ->
            QueuedOutboundDraft.create(
                ownerBareJid = OWNER_A,
                clientStanzaId = "q-$index",
                enqueuedAtMillis = 1_000L + index,
                payload = QueuedOutboundPayload(
                    target = QueuedOutboundTarget.Chat("alice@waddle.test"),
                    content = QueuedOutboundContent("message-$index"),
                ),
            ).persisted(
                sequence = index.toLong() + 1,
                ownership = OutboundOwnership.NativeOwned(attempt, phase),
            )
        }

        prefs.updateDeliveryJournal { journal ->
            val owner = checkNotNull(journal.owners[OWNER_A])
            DeliveryJournalMutation(
                journal.copy(
                    owners = journal.owners + (
                        OWNER_A to owner.copy(
                            activeAttempt = attempt,
                            nextSequence = rows.size.toLong() + 1,
                            outboundRows = rows,
                        )
                    ),
                ),
                Unit,
            )
        }

        val stored = prefs.deliveryJournal.first()
        assertEquals(attempt, stored.owners[OWNER_A]?.activeAttempt)
        assertEquals(rows, stored.owners[OWNER_A]?.outboundRows)
        assertEquals(
            NativeOutboundPhase.entries,
            stored.owners[OWNER_A]?.outboundRows?.map {
                (it.ownership as OutboundOwnership.NativeOwned).phase
            },
        )
    }

    @Test
    fun `direct reply round trip preserves target and partial annotations`() = runBlocking {
        val prefs = newPrefs()
        prefs.activateSession(OWNER_A, "sess-a")
        val payload = directReplyPayload()
        val source = DeliverySource.DirectReply(
            conversationJid = "general@muc.waddle.test",
            isGroupchat = true,
        )
        prefs.storeOutboundRow(directReplyRow(payload, source))

        val stored = prefs.deliveryJournal.first()
            .owners
            .getValue(OWNER_A)
            .outboundRows
            .single()
        assertEquals(source, stored.source)
        assertEquals(payload, stored.payload)
        assertNull(stored.payload.content.reply.authorJid)
        assertNull(stored.payload.content.thread.id)
        assertEquals(payload.content.sharedFiles, stored.payload.content.sharedFiles)
        assertEquals(payload.content.mentions, stored.payload.content.mentions)
    }

    @Test
    fun `clear purges only active owner and preserves foreign owner`() = runBlocking {
        val prefs = newPrefs()
        val suffix = prefs.resourceSuffix()
        prefs.activateSession(OWNER_A, "sess-a")
        prefs.activateSession(OWNER_B, "sess-b")
        prefs.setJoinedRooms(setOf("secret@muc.waddle.test"))

        prefs.clear()

        val journal = prefs.deliveryJournal.first()
        assertNull(journal.activeOwnerBareJid)
        assertTrue(OWNER_A in journal.owners)
        assertTrue(OWNER_B !in journal.owners)
        assertNull(prefs.sessionId.first())
        assertTrue(prefs.joinedRooms.first().isEmpty())
        assertEquals("per-install suffix survives logout", suffix, prefs.resourceSuffix())
    }

    @Test
    fun `resume cursors round trip and empty clears the key`() = runBlocking {
        val prefs = newPrefs()
        val cursors = mapOf(
            "alice@waddle.test" to ResumeCursor(
                stanzaId = "s-1",
                timestamp = "2026-07-15T10:00:00Z",
            ),
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

        prefs.activateSession(OWNER_A, "sess-a")
        prefs.clear()

        assertNull(prefs.sessionId.first())
        assertEquals("per-install suffix survives logout", suffix, prefs.resourceSuffix())
    }

    private fun directReplyPayload(): QueuedOutboundPayload = QueuedOutboundPayload(
        target = QueuedOutboundTarget.Groupchat("general@muc.waddle.test"),
        content = QueuedOutboundContent(
            body = "partial annotations",
            reply = QueuedOutboundReply(
                id = "reply-1",
                authorJid = null,
                parentBody = "parent body",
            ),
            thread = QueuedOutboundThread(
                id = null,
                parent = "thread-parent",
            ),
            sharedFiles = listOf(
                SharedFileRef(
                    url = "https://files.waddle.test/1",
                    name = null,
                    mediaType = "image/png",
                    sizeBytes = null,
                    disposition = FileDisposition.INLINE,
                ),
            ),
            mentions = listOf(
                MentionRef("xmpp:bob@waddle.test", 0u, 3u),
                MentionRef("xmpp:carol@waddle.test", 4u, 9u),
            ),
        ),
    )

    private fun directReplyRow(
        payload: QueuedOutboundPayload,
        source: DeliverySource,
    ): QueuedOutboundMessage = QueuedOutboundDraft.create(
        ownerBareJid = OWNER_A,
        clientStanzaId = "direct-reply-1",
        enqueuedAtMillis = 2_000,
        payload = payload,
        source = source,
    ).persisted(sequence = 1, ownership = OutboundOwnership.Ready)

    private suspend fun SessionPrefs.storeOutboundRow(row: QueuedOutboundMessage) {
        updateDeliveryJournal { journal ->
            val owner = checkNotNull(journal.owners[OWNER_A])
            DeliveryJournalMutation(
                journal.copy(
                    owners = journal.owners + (
                        OWNER_A to owner.copy(
                            nextSequence = 2,
                            outboundRows = listOf(row),
                        )
                    ),
                ),
                Unit,
            )
        }
    }

    private fun attempt(
        owner: String,
        id: String,
        generation: ULong,
    ): DeliveryAttemptRef = DeliveryAttemptRef(
        ownerBareJid = owner,
        attemptId = DeliveryAttemptId(id),
        nativeGeneration = NativeConnectionGeneration(generation),
    )

    private companion object {
        const val OWNER_A = "alice@waddle.test"
        const val OWNER_B = "bob@waddle.test"
    }
}
