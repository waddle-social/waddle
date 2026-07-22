package social.waddle.android.client.calls

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * XEP-0272 Muji presence bookkeeping, mirroring the web
 * muc-call-presence.ts suite: §Joining/§Leaving add/remove rules,
 * preparing-only semantics, media aggregation, owner mapping, the
 * preparing-echo waiter, and the no-other-preparing wait.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MucCallPresenceTest {
    private val presence = MucCallPresence()

    // ── Membership rules ─────────────────────────────────────────────────────

    @Test
    fun `active muji presence adds the nick with owner and media`() {
        presence.applyMucCallPresence(mujiPresence("alice", active = true, mucJid = OWN_FULL))

        assertEquals(mapOf(ROOM_JID to setOf("alice")), presence.participants.value)
        assertEquals(mapOf(ROOM_JID to mapOf<String, String?>("alice" to OWN_FULL)), presence.owners.value)
        assertEquals(mapOf(ROOM_JID to MucCallMedia(audio = true, video = false)), presence.media.value)
    }

    @Test
    fun `presence without muji removes the nick per the leaving marker`() {
        presence.applyMucCallPresence(mujiPresence("alice", active = true))

        presence.applyMucCallPresence(mujiPresence("alice"))

        assertTrue(presence.participants.value.isEmpty())
        assertTrue(presence.owners.value.isEmpty())
        assertTrue(presence.media.value.isEmpty())
    }

    @Test
    fun `unavailable presence removes the nick`() {
        presence.applyMucCallPresence(mujiPresence("alice", active = true))

        presence.applyMucCallPresence(
            mujiPresence("alice", active = true, presenceType = "unavailable"),
        )

        assertTrue(presence.participants.value.isEmpty())
    }

    @Test
    fun `preparing-only muji never counts as call membership`() {
        presence.applyMucCallPresence(mujiPresence("alice", preparing = true))

        assertTrue(presence.participants.value.isEmpty())

        // Preparing does not CLEAR active membership from a sibling
        // resource either — it is purely the setup signal.
        presence.applyMucCallPresence(mujiPresence("alice", active = true, mucJid = OWN_FULL))
        presence.applyMucCallPresence(mujiPresence("alice", preparing = true, mucJid = PEER_FULL))
        assertEquals(mapOf(ROOM_JID to setOf("alice")), presence.participants.value)
    }

    @Test
    fun `duplicate and replayed presences are idempotent`() {
        presence.applyMucCallPresence(mujiPresence("alice", active = true))
        presence.applyMucCallPresence(mujiPresence("alice", active = true))
        assertEquals(mapOf(ROOM_JID to setOf("alice")), presence.participants.value)

        presence.applyMucCallPresence(mujiPresence("ghost"))
        assertEquals(mapOf(ROOM_JID to setOf("alice")), presence.participants.value)
    }

    @Test
    fun `media aggregates across active participants`() {
        presence.applyMucCallPresence(mujiPresence("alice", active = true, mucJid = OWN_FULL))
        presence.applyMucCallPresence(
            mujiPresence("bob", active = true, hasVideo = true, mucJid = PEER_FULL),
        )

        assertEquals(mapOf(ROOM_JID to MucCallMedia(audio = true, video = true)), presence.media.value)

        presence.applyMucCallPresence(mujiPresence("bob", mucJid = PEER_FULL))
        assertEquals(mapOf(ROOM_JID to MucCallMedia(audio = true, video = false)), presence.media.value)
    }

    @Test
    fun `removal is keyed by real jid when the room exposes it`() {
        presence.applyMucCallPresence(mujiPresence("alice", active = true, mucJid = OWN_FULL))
        presence.applyMucCallPresence(mujiPresence("alice", active = true, mucJid = PEER_FULL))

        // Dropping one resource's advertisement keeps the sibling's.
        presence.applyMucCallPresence(mujiPresence("alice", mucJid = OWN_FULL))
        assertEquals(mapOf(ROOM_JID to setOf("alice")), presence.participants.value)

        presence.applyMucCallPresence(mujiPresence("alice", mucJid = PEER_FULL))
        assertTrue(presence.participants.value.isEmpty())
    }

    @Test
    fun `clearParticipant drops the local occupant without an inbound presence`() {
        presence.applyMucCallPresence(mujiPresence("alice", active = true, mucJid = OWN_FULL))

        presence.clearParticipant(ROOM_JID, "alice", OWN_FULL)

        assertTrue(presence.participants.value.isEmpty())
    }

    // ── In-call flags ────────────────────────────────────────────────────────

    @Test
    fun `raised hand and mute markers track the presence flags`() {
        presence.applyMucCallPresence(
            mujiPresence("alice", active = true, handRaised = true, muted = true),
        )
        assertEquals(mapOf(ROOM_JID to setOf("alice")), presence.raisedHands.value)
        assertEquals(mapOf(ROOM_JID to setOf("alice")), presence.mutedNicks.value)

        presence.applyMucCallPresence(mujiPresence("alice", active = true, handRaised = false, muted = true))
        assertTrue(presence.raisedHands.value.isEmpty())
        assertEquals(mapOf(ROOM_JID to setOf("alice")), presence.mutedNicks.value)

        // Leaving the call clears all in-call state.
        presence.applyMucCallPresence(mujiPresence("alice"))
        assertTrue(presence.mutedNicks.value.isEmpty())
    }

    // ── Preparing-echo waiter ────────────────────────────────────────────────

    @Test
    fun `preparing echo resolves a waiter registered before the send`() = runTest {
        val waiter = presence.registerPreparingEchoWaiter(ROOM_JID, "alice", OWN_FULL)
        val wait = async { presence.awaitPreparingEcho(waiter, 2_000) }
        runCurrent()

        presence.applyMucCallPresence(mujiPresence("alice", preparing = true, mucJid = OWN_FULL))

        assertTrue(wait.await())
    }

    @Test
    fun `preparing echo waiter times out without an echo`() = runTest {
        val waiter = presence.registerPreparingEchoWaiter(ROOM_JID, "alice", OWN_FULL)
        val wait = async { presence.awaitPreparingEcho(waiter, 2_000) }
        runCurrent()

        advanceTimeBy(2_001)

        assertFalse(wait.await())
    }

    @Test
    fun `a sibling resource's echo does not satisfy our identity-keyed waiter`() = runTest {
        val waiter = presence.registerPreparingEchoWaiter(ROOM_JID, "alice", OWN_FULL)
        val wait = async { presence.awaitPreparingEcho(waiter, 2_000) }
        runCurrent()

        presence.applyMucCallPresence(mujiPresence("alice", preparing = true, mucJid = PEER_FULL))
        runCurrent()
        assertFalse(wait.isCompleted)

        advanceTimeBy(2_001)
        assertFalse(wait.await())
    }

    @Test
    fun `cancelPreparationWaiters fails a pending echo wait`() = runTest {
        val waiter = presence.registerPreparingEchoWaiter(ROOM_JID, "alice", OWN_FULL)
        val wait = async { presence.awaitPreparingEcho(waiter, 2_000) }
        runCurrent()

        presence.cancelPreparationWaiters(ROOM_JID, "alice")

        assertFalse(wait.await())
    }

    // ── No-other-preparing wait ──────────────────────────────────────────────

    @Test
    fun `awaitNoOtherPreparing resolves immediately when nobody is preparing`() = runTest {
        assertTrue(presence.awaitNoOtherPreparing(ROOM_JID, "alice", OWN_FULL, 2_000))
    }

    @Test
    fun `awaitNoOtherPreparing waits for the other occupant to finish preparation`() = runTest {
        presence.applyMucCallPresence(mujiPresence("bob", preparing = true, mucJid = PEER_FULL))
        val wait = async { presence.awaitNoOtherPreparing(ROOM_JID, "alice", OWN_FULL, 2_000) }
        runCurrent()
        assertFalse(wait.isCompleted)

        // Bob finishes preparing (contents declared, preparing dropped).
        presence.applyMucCallPresence(mujiPresence("bob", active = true, mucJid = PEER_FULL))

        assertTrue(wait.await())
    }

    @Test
    fun `awaitNoOtherPreparing times out while the other keeps preparing`() = runTest {
        presence.applyMucCallPresence(mujiPresence("bob", preparing = true, mucJid = PEER_FULL))
        val wait = async { presence.awaitNoOtherPreparing(ROOM_JID, "alice", OWN_FULL, 2_000) }
        runCurrent()

        advanceTimeBy(2_001)

        assertFalse(wait.await())
    }

    @Test
    fun `our own preparing entry does not block the no-other wait`() = runTest {
        presence.applyMucCallPresence(mujiPresence("alice", preparing = true, mucJid = OWN_FULL))

        assertTrue(presence.awaitNoOtherPreparing(ROOM_JID, "alice", OWN_FULL, 2_000))
    }
}
