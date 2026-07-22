package social.waddle.android.client.calls

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.RecordedCallVerb

/**
 * The MUC session resume cache and its store-level consumers,
 * mirroring the web muc-call-session-cache.ts + muc-call-actions.ts
 * suites: resume promotes the slot straight to Active with the cached
 * join (no fresh initiate), stale/mismatched entries refuse, and the
 * retained-leave path clears presence before the cached-sid terminate.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MucCallSessionCacheTest {
    private val now = 1_000_000L

    private val leavePresenceVerb = RecordedCallVerb.UpdateMujiPresence(
        roomJid = ROOM_JID, nick = SELF_NICK,
        active = false, preparing = false, video = false,
        handRaised = false, muted = false,
    )

    private suspend fun rememberSession(f: Fixture, sid: String = "c-old") {
        f.sessionCache.remember(ROOM_JID, sid, OWN_FULL, audio, mucJoin, nowMillis = now)
    }

    // ── Cache semantics ──────────────────────────────────────────────────────

    @Test
    fun `remember and read roundtrip enables resume`() = runTest {
        val f = Fixture()
        rememberSession(f)

        val entry = f.sessionCache.read(ROOM_JID, OWN_FULL, nowMillis = now + 1)
        assertEquals("c-old", entry?.sid)
        assertEquals(mucJoin, entry?.join())
        assertTrue(f.sessionCache.canResume(ROOM_JID, OWN_FULL, nowMillis = now + 1))
    }

    @Test
    fun `entries staler than the 24h window refuse to resume`() = runTest {
        val f = Fixture()
        rememberSession(f)

        val later = now + MucCallSessionCache.CACHE_WINDOW_MILLIS + 1
        assertNull(f.sessionCache.read(ROOM_JID, OWN_FULL, nowMillis = later))
        assertFalse(f.sessionCache.canResume(ROOM_JID, OWN_FULL, nowMillis = later))
    }

    @Test
    fun `an identity mismatch refuses to resume`() = runTest {
        val f = Fixture()
        f.sessionCache.remember(
            ROOM_JID, "c-old", OWN_FULL, audio,
            mucJoin.copy(identity = PEER_FULL),
            nowMillis = now,
        )

        assertFalse(f.sessionCache.canResume(ROOM_JID, OWN_FULL, nowMillis = now + 1))
        // And a different resource never sees another identity's entry.
        assertNull(f.sessionCache.read(ROOM_JID, PEER_FULL, nowMillis = now + 1))
    }

    // ── Resume ───────────────────────────────────────────────────────────────

    @Test
    fun `resume promotes the slot to Active without a fresh initiate`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        rememberSession(f)

        assertTrue(f.store.muc.resume(ROOM_JID, SELF_NICK, OWN_FULL, nowMillis = now + 1))
        runCurrent()

        assertEquals(
            CallState.Active(
                peer = ROOM_JID, sid = "c-old", media = audio, join = mucJoin,
                kind = CallKind.MUC, selfNick = SELF_NICK,
            ),
            f.store.state.value,
        )
        // No new Jingle attempt — the cached join reconnects directly;
        // only the best-effort active presence re-publish goes out.
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.MujiSessionInitiate })
        assertEquals(
            listOf<RecordedCallVerb>(
                RecordedCallVerb.UpdateMujiPresence(
                    roomJid = ROOM_JID, nick = SELF_NICK,
                    active = true, preparing = false, video = false,
                    handRaised = false, muted = false,
                ),
            ),
            f.client.callVerbs,
        )
    }

    @Test
    fun `resume refuses without a usable cache entry or while the slot is busy`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        assertFalse(f.store.muc.resume(ROOM_JID, SELF_NICK, OWN_FULL, nowMillis = now))

        rememberSession(f)
        assertTrue(f.store.startCall(PEER_BARE, audio))
        assertFalse(f.store.muc.resume(ROOM_JID, SELF_NICK, OWN_FULL, nowMillis = now + 1))
        assertTrue(f.store.state.value is CallState.Outgoing)
    }

    // ── Retained leave ───────────────────────────────────────────────────────

    @Test
    fun `leaveRetained clears presence then terminates the cached sid`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        rememberSession(f)

        assertTrue(f.store.muc.leaveRetained(ROOM_JID, SELF_NICK, OWN_FULL, nowMillis = now + 1))

        assertEquals(
            listOf(
                leavePresenceVerb,
                RecordedCallVerb.MujiSessionTerminate(ROOM_JID, "c-old"),
            ),
            f.client.callVerbs,
        )
        assertNull(f.sessionCache.read(ROOM_JID, OWN_FULL, nowMillis = now + 2))
    }

    @Test
    fun `leaveRetained without a cached session only clears presence`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        assertTrue(f.store.muc.leaveRetained(ROOM_JID, SELF_NICK, OWN_FULL, nowMillis = now))

        assertEquals(listOf<RecordedCallVerb>(leavePresenceVerb), f.client.callVerbs)
    }

    @Test
    fun `a failed retained terminate marks the entry terminate-pending`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        rememberSession(f)
        f.client.mujiSessionTerminateFailure = RuntimeException("boom")

        assertFalse(f.store.muc.leaveRetained(ROOM_JID, SELF_NICK, OWN_FULL, nowMillis = now + 1))

        val entry = f.sessionCache.read(ROOM_JID, OWN_FULL, nowMillis = now + 2)
        assertTrue(entry?.terminatePending == true)
        assertFalse(f.sessionCache.canResume(ROOM_JID, OWN_FULL, nowMillis = now + 2))
    }

    @Test
    fun `a successful hangUp forgets the remembered session`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = async {
            f.store.muc.begin(ROOM_JID, audio, SELF_NICK, OWN_FULL, MIXER_JID)
        }
        runCurrent()
        f.store.onPresence(mujiPresence(SELF_NICK, preparing = true, mucJid = OWN_FULL))
        runCurrent()
        f.store.onCallEvent(mucSessionAccept("c-fixed"))
        runCurrent()
        assertTrue(started.await())
        assertEquals("c-fixed", f.sessionCache.read(ROOM_JID, OWN_FULL)?.sid)

        f.store.hangUp()

        assertNull(f.sessionCache.read(ROOM_JID, OWN_FULL))
    }
}
