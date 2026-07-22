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
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.RecordedCallVerb
import social.waddle.client.ffi.WaddleJingleReason

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
    fun `a hangUp leave racing the preparing presence lands AFTER it on the wire`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        // Stall ONLY the preparing presence; the concurrent teardown's
        // leave must queue behind it on the presence mutex instead of
        // overtaking it (a leave-then-preparing inversion would plant a
        // permanent room-visible <preparing/> ghost).
        f.client.updateMujiPresenceDelaysMillis += 50L

        val started = begin(f, this)
        f.store.hangUp()
        runCurrent()
        advanceTimeBy(51)
        runCurrent()

        assertFalse(started.await())
        assertEquals(CallState.Idle, f.store.state.value)
        // The stalled preparing presence lands first, then the
        // teardown's leave, then the abandoned attempt's idempotent
        // re-clear — never an active/preparing marker after a leave.
        assertEquals(
            listOf(preparingPresenceVerb, leavePresenceVerb, leavePresenceVerb),
            f.client.callVerbs,
        )
    }

    @Test
    fun `a teardown claiming the slot after the active presence suppresses the initiate`() = runTest {
        // The initiate step's bound-resource lookup runs between the
        // active-presence CAS and the wire send — the exact window a
        // concurrent hangUp can win. Claim the slot there to model it.
        var hijack: CallStore? = null
        val f = Fixture(ownFullJid = {
            hijack?.updateCallSlot { CallState.Idle to Unit }
            hijack = null
            OWN_FULL
        })
        f.store.start(backgroundScope)
        hijack = f.store
        val started = begin(f, this)

        echoOwnPreparing(f)
        runCurrent()

        assertFalse(started.await())
        assertEquals(CallState.Idle, f.store.state.value)
        // The teardown owner already terminated this sid: a fresh
        // initiate would re-open a mixer session nothing tracks.
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.MujiSessionInitiate })
    }

    @Test
    fun `an accept that raced a teardown re-terminates the fresh mixer session`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        val started = begin(f, this)
        echoOwnPreparing(f)

        // The mixer's accept resolves the waiter, but the teardown
        // claims the slot before the engine coroutine resumes: the
        // accepted session provably post-dates the teardown-owned
        // terminate and must be closed again.
        f.store.onCallEvent(mucSessionAccept("c-fixed"))
        f.store.hangUp()
        runCurrent()

        assertFalse(started.await())
        assertEquals(CallState.Idle, f.store.state.value)
        assertEquals(
            listOf(
                preparingPresenceVerb, activePresenceVerb, initiateVerb,
                leavePresenceVerb, terminateVerb, terminateVerb,
            ),
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
        // Two leaves: the teardown's, plus the abandoned prepare step's
        // unconditional re-clear (it cannot know whether its preparing
        // presence outraced the teardown's leave on the wire) — and
        // still NO terminate for the never-advertised session.
        assertEquals(
            listOf(preparingPresenceVerb, leavePresenceVerb, leavePresenceVerb),
            f.client.callVerbs,
        )
    }

    // ── Remote end events (engine-owned teardown) ────────────────────────────

    @Test
    fun `a mixer session-terminate ends the MUC call through the engine teardown`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        assertEquals("c-fixed", f.sessionCache.read(ROOM_JID, OWN_FULL)?.sid)

        f.store.onCallEvent(
            sessionTerminate("$MIXER_JID/mixer", "c-fixed", WaddleJingleReason.SUCCESS),
        )
        runCurrent()

        assertEquals(
            CallState.Ended(sid = "c-fixed", reason = CallEndReason.Finished(WaddleJingleReason.SUCCESS)),
            f.store.state.value,
        )
        // XEP-0272 §Leaving order even for a remote-initiated end:
        // presence-clear first, then the (idempotent) mixer terminate.
        assertEquals(
            listOf(preparingPresenceVerb, activePresenceVerb, initiateVerb, leavePresenceVerb, terminateVerb),
            f.client.callVerbs,
        )
        assertNull(f.sessionCache.read(ROOM_JID, OWN_FULL))
    }

    @Test
    fun `a mismatched-sid terminate never touches the live MUC call`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        val active = f.store.state.value

        f.store.onCallEvent(
            sessionTerminate("$MIXER_JID/mixer", "c-stale", WaddleJingleReason.SUCCESS),
        )
        runCurrent()

        assertEquals(active, f.store.state.value)
        assertTrue(f.client.callVerbs.none { it == leavePresenceVerb })
    }

    @Test
    fun `a failed leave send never marks the self-leave echo pending`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        f.client.updateMujiPresenceFailure = RuntimeException("boom")

        f.store.hangUp()
        runCurrent()

        // No echo will ever settle a failed leave: the marker must stay
        // clear so the retained-leave recovery remains reachable for
        // the durable ghost the failure leaves behind.
        assertTrue(f.store.mucCallPresence.selfLeaveEchoPending.value.isEmpty())
    }

    @Test
    fun `a stale raised hand never rides into a resumed call's re-stamp`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        assertTrue(f.store.muc.setHandRaised(true))
        val otherRoom = "lounge@muc.waddle.test"
        f.sessionCache.remember(
            otherRoom, "c-b", OWN_FULL, audio,
            mucJoin.copy(room = otherRoom),
            nowMillis = 1_000L,
        )
        // Stall the teardown's leave so the resumed attempt claims the
        // slot before the leave's flag-reset check runs (the reset is
        // then skipped — resume's own claim-time init must zero it).
        f.client.updateMujiPresenceDelaysMillis += 500L
        val hungUp = backgroundScope.async { f.store.hangUp() }
        runCurrent()
        assertTrue(f.store.muc.resume(otherRoom, SELF_NICK, OWN_FULL, nowMillis = 1_001L))
        advanceTimeBy(501)
        runCurrent()
        hungUp.await()

        val restamp = f.client.callVerbs
            .filterIsInstance<RecordedCallVerb.UpdateMujiPresence>()
            .last { it.roomJid == otherRoom && it.active }
        assertFalse(restamp.handRaised)
        assertFalse(f.store.muc.selfHandRaised.value)
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
    fun `racing hand toggles serialize raise-then-lower on the wire`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        // Stall only the FIRST re-stamp; the second must still queue
        // behind it instead of overtaking on the wire.
        f.client.updateMujiPresenceDelaysMillis += 50L

        val raise = backgroundScope.async { f.store.muc.setHandRaised(true) }
        runCurrent()
        // Optimistic flip is visible BEFORE the send lands (web parity).
        assertTrue(f.store.muc.selfHandRaised.value)
        val lower = backgroundScope.async { f.store.muc.setHandRaised(false) }
        runCurrent()
        advanceTimeBy(51)
        runCurrent()

        assertTrue(raise.await())
        assertTrue(lower.await())
        assertFalse(f.store.muc.selfHandRaised.value)
        assertEquals(
            listOf(
                activePresenceVerb.copy(handRaised = true),
                activePresenceVerb.copy(handRaised = false),
            ),
            f.client.callVerbs.takeLast(2),
        )
    }

    @Test
    fun `an interleaved mute broadcast and hand raise serialize with the newest flags`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        f.client.updateMujiPresenceDelaysMillis += 50L

        val mute = backgroundScope.async { f.store.muc.broadcastSelfMute(true) }
        runCurrent()
        val raise = backgroundScope.async { f.store.muc.setHandRaised(true) }
        runCurrent()
        advanceTimeBy(51)
        runCurrent()

        assertTrue(mute.await())
        assertTrue(raise.await())
        // The hand re-stamp queued behind the mute and carries it.
        assertEquals(
            listOf(
                activePresenceVerb.copy(muted = true),
                activePresenceVerb.copy(handRaised = true, muted = true),
            ),
            f.client.callVerbs.takeLast(2),
        )
    }

    @Test
    fun `re-stamps parked behind an in-flight send carry the final optimistic state`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        f.client.updateMujiPresenceDelaysMillis += 50L

        val mute = backgroundScope.async { f.store.muc.broadcastSelfMute(true) }
        runCurrent()
        // Two racing hand toggles PARK behind the in-flight mute: both
        // snapshot the flags INSIDE the send mutex, so whatever order
        // they acquire it in, both send the identical FINAL state —
        // the wire can never end on a value the UI no longer shows.
        val raise = backgroundScope.async { f.store.muc.setHandRaised(true) }
        runCurrent()
        val lower = backgroundScope.async { f.store.muc.setHandRaised(false) }
        runCurrent()
        advanceTimeBy(51)
        runCurrent()

        assertTrue(mute.await())
        assertTrue(raise.await())
        assertTrue(lower.await())
        assertFalse(f.store.muc.selfHandRaised.value)
        assertEquals(
            listOf(
                activePresenceVerb.copy(muted = true),
                activePresenceVerb.copy(handRaised = false, muted = true),
                activePresenceVerb.copy(handRaised = false, muted = true),
            ),
            f.client.callVerbs.takeLast(3),
        )
    }

    @Test
    fun `a parked re-stamp skips once a hangUp leave claims the slot`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        f.client.updateMujiPresenceDelaysMillis += 50L

        val raise = backgroundScope.async { f.store.muc.setHandRaised(true) }
        runCurrent()
        // Parks behind the in-flight raise while the slot is still
        // Active…
        val mute = backgroundScope.async { f.store.muc.broadcastSelfMute(true) }
        runCurrent()
        // …then the teardown claims the slot. Its leave queues on the
        // SAME mutex, and the parked re-stamp must re-check the slot
        // and stand down instead of resurrecting the active
        // advertisement after the leave.
        val hangUp = backgroundScope.async { f.store.hangUp() }
        runCurrent()
        advanceTimeBy(51)
        runCurrent()

        assertTrue(raise.await())
        assertFalse(mute.await())
        hangUp.await()
        assertEquals(CallState.Idle, f.store.state.value)
        // XEP-0272 §Leaving pinned on the wire: the in-flight re-stamp,
        // the leave, the mixer terminate — and nothing after the leave.
        assertEquals(
            listOf(activePresenceVerb.copy(handRaised = true), leavePresenceVerb, terminateVerb),
            f.client.callVerbs.takeLast(3),
        )
    }

    @Test
    fun `an abandoned attempt's re-clear never wipes a newer same-room attempt`() = runTest {
        var counter = 0
        val f = Fixture(sid = { "c-${counter++}" })
        f.store.start(backgroundScope)
        // Stall attempt A's preparing presence so its abandoned-setup
        // re-clear decision happens AFTER attempt B claimed the room.
        f.client.updateMujiPresenceDelaysMillis += 50L

        val a = backgroundScope.async { f.store.muc.begin(ROOM_JID, audio, SELF_NICK, OWN_FULL, MIXER_JID) }
        runCurrent()
        val hangUp = backgroundScope.async { f.store.hangUp() }
        runCurrent()
        val b = backgroundScope.async { f.store.muc.begin(ROOM_JID, audio, SELF_NICK, OWN_FULL, MIXER_JID) }
        runCurrent()
        advanceTimeBy(51)
        runCurrent()
        // B's setup proceeds normally: preparing echo, mixer accept.
        f.store.onPresence(mujiPresence(SELF_NICK, preparing = true, mucJid = OWN_FULL))
        runCurrent()
        f.store.onCallEvent(mucSessionAccept("c-1"))
        runCurrent()

        assertFalse(a.await())
        hangUp.await()
        assertTrue(b.await())
        assertTrue(f.store.state.value is CallState.Active)
        // Neither A's teardown nor its abandoned re-clear may wipe the
        // room B now owns: no leave presence lands at all.
        assertTrue(f.client.callVerbs.none { it == leavePresenceVerb })
    }

    @Test
    fun `a failed hand-raise send rolls the optimistic flag back`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        assertTrue(activateMucCall(f).await())
        f.client.updateMujiPresenceFailure = RuntimeException("boom")

        assertFalse(f.store.muc.setHandRaised(true))

        assertFalse(f.store.muc.selfHandRaised.value)
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
