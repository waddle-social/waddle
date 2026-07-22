package social.waddle.android.client.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.RecordedCallVerb

/**
 * The XEP-0272 MUC group-call flow over the shared call slot,
 * mirroring the web call-store suite's beginMucCall coverage: the
 * pinned §Joining verb order, the separate-IQ accept correlation, the
 * presence-first rollback/teardown ordering, in-call flag re-stamps,
 * and DM/group mutual exclusion.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class CallStoreMucTest {
    private val preparingPresenceVerb = RecordedCallVerb.UpdateMujiPresence(
        roomJid = ROOM_JID, nick = SELF_NICK,
        active = false, preparing = true, video = false,
        handRaised = false, muted = false,
    )
    private val activePresenceVerb = RecordedCallVerb.UpdateMujiPresence(
        roomJid = ROOM_JID, nick = SELF_NICK,
        active = true, preparing = false, video = false,
        handRaised = false, muted = false,
    )
    private val leavePresenceVerb = RecordedCallVerb.UpdateMujiPresence(
        roomJid = ROOM_JID, nick = SELF_NICK,
        active = false, preparing = false, video = false,
        handRaised = false, muted = false,
    )
    private val initiateVerb =
        RecordedCallVerb.MujiSessionInitiate(ROOM_JID, OWN_FULL, "c-fixed", video = false)
    private val terminateVerb = RecordedCallVerb.MujiSessionTerminate(ROOM_JID, "c-fixed")

    private fun TestScope.begin(f: Fixture, scope: CoroutineScope): Deferred<Boolean> {
        val started = scope.async { f.store.muc.begin(ROOM_JID, audio, SELF_NICK, OWN_FULL, MIXER_JID) }
        runCurrent()
        return started
    }

    private fun TestScope.echoOwnPreparing(f: Fixture) {
        f.store.onPresence(mujiPresence(SELF_NICK, preparing = true, mucJid = OWN_FULL))
        runCurrent()
    }

    private fun TestScope.activateMucCall(f: Fixture): Deferred<Boolean> {
        val started = begin(f, this)
        echoOwnPreparing(f)
        f.store.onCallEvent(mucSessionAccept("c-fixed"))
        runCurrent()
        return started
    }

    // ── Happy path ───────────────────────────────────────────────────────────

    @Test
    fun `beginMucCall pins the joining verb order and activates on the mixer accept`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        val started = activateMucCall(f)

        assertTrue(started.await())
        // XEP-0272 §Joining: preparing presence → (echo) → active
        // content presence → Jingle session-initiate, in that order.
        assertEquals(listOf(preparingPresenceVerb, activePresenceVerb, initiateVerb), f.client.callVerbs)
        val state = f.store.state.value
        assertEquals(
            CallState.Active(
                peer = ROOM_JID, sid = "c-fixed", media = audio, join = mucJoin,
                kind = CallKind.MUC, selfNick = SELF_NICK,
            ),
            state,
        )
    }

    @Test
    fun `joining without mic capture advertises muted on the active presence`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val noMic = social.waddle.client.ffi.WaddleCallMedia(audio = false, video = false)

        val started = backgroundScope.async {
            f.store.muc.begin(ROOM_JID, noMic, SELF_NICK, OWN_FULL, MIXER_JID)
        }
        runCurrent()
        echoOwnPreparing(f)
        f.store.onCallEvent(mucSessionAccept("c-fixed"))
        runCurrent()

        assertTrue(started.await())
        assertTrue(activePresenceVerb.copy(muted = true) in f.client.callVerbs)
    }

    // ── Accept correlation ───────────────────────────────────────────────────

    @Test
    fun `a stale-sid accept is ignored and the matching one still activates`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = begin(f, this)
        echoOwnPreparing(f)

        f.store.onCallEvent(mucSessionAccept("c-stale"))
        runCurrent()
        assertTrue(f.store.state.value is CallState.MucPending)

        f.store.onCallEvent(mucSessionAccept("c-fixed"))
        runCurrent()
        assertTrue(started.await())
        assertTrue(f.store.state.value is CallState.Active)
    }

    @Test
    fun `an accept for the wrong room is consumed without resolving the attempt`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = begin(f, this)
        echoOwnPreparing(f)

        f.store.onCallEvent(
            mucSessionAccept("c-fixed", accepted = mucJoin.copy(room = "other@muc.waddle.test")),
        )
        runCurrent()
        assertTrue(f.store.state.value is CallState.MucPending)

        f.store.onCallEvent(mucSessionAccept("c-fixed"))
        runCurrent()
        assertTrue(started.await())
    }

    @Test
    fun `an accept from the wrong mixer is consumed without resolving the attempt`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = begin(f, this)
        echoOwnPreparing(f)

        f.store.onCallEvent(mucSessionAccept("c-fixed", from = "evil.waddle.test/mixer"))
        runCurrent()
        assertTrue(f.store.state.value is CallState.MucPending)

        f.store.onCallEvent(mucSessionAccept("c-fixed", from = "$MIXER_JID/mixer"))
        runCurrent()
        assertTrue(started.await())
    }

    @Test
    fun `a replayed muji accept never clobbers the live slot through the reducer`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = activateMucCall(f)
        assertTrue(started.await())
        val active = f.store.state.value

        // The pending resolver is gone; the replay falls through to the
        // 1:1 reducer, which must drop it (phase/sid guarded).
        f.store.onCallEvent(mucSessionAccept("c-fixed"))
        runCurrent()

        assertEquals(active, f.store.state.value)
    }

    // ── Rollback ─────────────────────────────────────────────────────────────

    @Test
    fun `echo timeout rolls back with a leave presence and NO terminate`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = begin(f, this)

        advanceTimeBy(2_001)
        runCurrent()

        assertFalse(started.await())
        assertEquals(CallState.Idle, f.store.state.value)
        assertEquals(listOf(preparingPresenceVerb, leavePresenceVerb), f.client.callVerbs)
    }

    @Test
    fun `failure after the active presence rolls back with leave AND terminate`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.client.mujiSessionInitiateFailure = RuntimeException("boom")
        val started = begin(f, this)

        echoOwnPreparing(f)

        assertFalse(started.await())
        assertEquals(CallState.Idle, f.store.state.value)
        assertEquals(
            listOf(preparingPresenceVerb, activePresenceVerb, initiateVerb, leavePresenceVerb, terminateVerb),
            f.client.callVerbs,
        )
    }

    @Test
    fun `accept timeout rolls back with leave AND terminate`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = begin(f, this)
        echoOwnPreparing(f)

        advanceTimeBy(10_001)
        runCurrent()

        assertFalse(started.await())
        assertEquals(CallState.Idle, f.store.state.value)
        assertEquals(
            listOf(preparingPresenceVerb, activePresenceVerb, initiateVerb, leavePresenceVerb, terminateVerb),
            f.client.callVerbs,
        )
    }

    // ── Teardown ─────────────────────────────────────────────────────────────

    @Test
    fun `hangUp clears the muji presence BEFORE terminating the mixer session`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = activateMucCall(f)
        assertTrue(started.await())

        f.store.hangUp()

        assertEquals(CallState.Idle, f.store.state.value)
        // XEP-0272 §Leaving: presence-clear first, then the terminate.
        assertEquals(
            listOf(preparingPresenceVerb, activePresenceVerb, initiateVerb, leavePresenceVerb, terminateVerb),
            f.client.callVerbs,
        )
    }

    @Test
    fun `hangUp during setup leaves without terminating an unadvertised session`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = begin(f, this)

        f.store.hangUp()
        runCurrent()

        assertFalse(started.await())
        assertEquals(CallState.Idle, f.store.state.value)
        assertEquals(listOf(preparingPresenceVerb, leavePresenceVerb), f.client.callVerbs)
    }

    // ── In-call flag re-stamps ───────────────────────────────────────────────

    @Test
    fun `hand raise re-stamps BOTH in-call flags on the active presence`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        assertTrue(f.store.muc.broadcastSelfMute(true))

        assertTrue(f.store.muc.setHandRaised(true))

        assertEquals(
            activePresenceVerb.copy(handRaised = true, muted = true),
            f.client.callVerbs.last(),
        )
    }

    @Test
    fun `mute broadcast preserves the raised hand`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        assertTrue(f.store.muc.setHandRaised(true))

        assertTrue(f.store.muc.broadcastSelfMute(false))

        assertEquals(
            activePresenceVerb.copy(handRaised = true, muted = false),
            f.client.callVerbs.last(),
        )
    }

    @Test
    fun `flag re-stamps are refused outside an active MUC call`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        assertFalse(f.store.muc.setHandRaised(true))
        assertFalse(f.store.muc.broadcastSelfMute(true))

        // A DM call is not a group call either.
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        f.store.onCallEvent(sessionInitiate(PEER_FULL, "c1"))
        runCurrent()
        assertTrue(f.store.state.value is CallState.Active)
        assertFalse(f.store.muc.setHandRaised(true))
    }

    // ── Mutual exclusion (single shared slot) ────────────────────────────────

    @Test
    fun `beginMucCall is refused while a DM call holds the slot`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(f.store.startCall(PEER_BARE, audio))

        assertFalse(f.store.muc.begin(ROOM_JID, audio, SELF_NICK, OWN_FULL, MIXER_JID))

        assertTrue(f.store.state.value is CallState.Outgoing)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.UpdateMujiPresence })
    }

    @Test
    fun `startCall is refused while the group-call setup holds the slot`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = begin(f, this)

        assertFalse(f.store.startCall(PEER_BARE, audio))
        assertTrue(f.store.state.value is CallState.MucPending)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Propose })

        advanceTimeBy(2_001)
        assertFalse(started.await())
    }
}
