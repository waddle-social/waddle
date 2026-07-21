package social.waddle.android.client.calls

import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.FakeWaddleClient
import social.waddle.android.client.RecordedCallVerb
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleJingleReason

/**
 * The call state machine, mirroring the web reducer suite
 * (chat/src/lib/calls tests): propose/proceed/accept/terminate, both
 * XEP-0353 tie-break branches, busy-reject, ringing emission, the
 * auto-retract timeout, and the self-originated-carbon filter — all
 * through the REAL `ClientCallSignaling` wire path into the recording
 * [FakeWaddleClient].
 */
class CallStoreTest {
    // ── Incoming ring ────────────────────────────────────────────────────────

    @Test
    fun `inbound propose surfaces incoming and rings back the caller's bare jid`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        f.store.onCallEvent(propose(PEER_FULL, "c1", video))
        runCurrent()

        val state = f.store.state.value
        assertEquals(CallState.Incoming(from = PEER_FULL, sid = "c1", media = video), state)
        // XEP-0353 §3.2: <ringing/> to the caller's BARE JID.
        assertEquals(listOf<RecordedCallVerb>(RecordedCallVerb.Ringing(PEER_BARE, "c1")), f.client.callVerbs)
    }

    @Test
    fun `duplicate propose for the ringing sid does not ring twice`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        assertEquals(1, f.client.callVerbs.count { it is RecordedCallVerb.Ringing })
    }

    @Test
    fun `accept sends proceed to the proposer's full jid`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        assertTrue(f.store.acceptIncoming())

        assertTrue(RecordedCallVerb.Proceed(PEER_FULL, "c1") in f.client.callVerbs)
        val state = f.store.state.value
        assertTrue(state is CallState.Incoming && state.accepting)
    }

    @Test
    fun `decline sends reject to the proposer's full jid and clears the slot`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        assertTrue(f.store.declineIncoming())

        assertTrue(RecordedCallVerb.Reject(PEER_FULL, "c1") in f.client.callVerbs)
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `session-initiate while incoming goes active and sends session-accept as our resource`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        f.store.acceptIncoming()

        f.store.onCallEvent(sessionInitiate(PEER_FULL, "c1"))
        runCurrent()

        val state = f.store.state.value
        assertTrue(state is CallState.Active)
        state as CallState.Active
        assertEquals(PEER_FULL, state.peer)
        assertEquals(join, state.join)
        assertEquals(PEER_FULL, state.initiator)
        // XEP-0166 §6.2: the responder confirms with session-accept;
        // responder attribute is OUR full JID.
        assertTrue(RecordedCallVerb.SessionAccept(PEER_FULL, OWN_FULL, "c1", true, false) in f.client.callVerbs)
    }

    @Test
    fun `stale session-initiate for another sid is dropped`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        f.store.onCallEvent(sessionInitiate(PEER_FULL, "other-sid"))
        runCurrent()

        assertTrue(f.store.state.value is CallState.Incoming)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.SessionAccept })
    }

    // ── Outgoing call ────────────────────────────────────────────────────────

    @Test
    fun `startCall proposes to the bare jid and records the initiator`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)

        assertTrue(f.store.startCall("$PEER_BARE/ignored-resource", audio))

        assertEquals(
            listOf<RecordedCallVerb>(RecordedCallVerb.Propose(PEER_BARE, "c-out", true, false)),
            f.client.callVerbs,
        )
        assertEquals(
            CallState.Outgoing(to = PEER_BARE, sid = "c-out", media = audio, initiator = OWN_FULL),
            f.store.state.value,
        )
    }

    @Test
    fun `startCall is refused while another call is live`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        assertFalse(f.store.startCall("carol@waddle.test", audio))
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Propose })
    }

    @Test
    fun `failed propose rolls the slot back to idle`() = runTest {
        val f = Fixture()
        f.client.callVerbResult = false
        f.store.start(backgroundScope)

        assertFalse(f.store.startCall(PEER_BARE, audio))

        assertEquals(CallState.Idle, f.store.state.value)
        assertEquals("call propose failed", f.store.lastError.value)
    }

    @Test
    fun `ringing from the callee marks the outgoing slot ringing`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        f.store.onCallEvent(ringing(PEER_FULL, "c-out"))

        val state = f.store.state.value
        assertTrue(state is CallState.Outgoing && state.ringing)
    }

    @Test
    fun `proceed fires session-initiate to the responder resource and arms the accept timeout`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, video)

        f.store.onCallEvent(proceed(PEER_FULL, "c-out"))
        runCurrent()

        // XEP-0353 §0.6: session-initiate goes to the full JID stamped
        // on the proceed; initiator names our own resource (§7.1).
        assertTrue(
            RecordedCallVerb.SessionInitiate(PEER_FULL, OWN_FULL, "c-out", true, true) in f.client.callVerbs,
        )

        // The accept-gap timeout: no session-accept → terminate + Ended.
        advanceTimeBy(CallStore.SESSION_ACCEPT_TIMEOUT_MILLIS + 1)
        runCurrent()
        assertTrue(
            RecordedCallVerb.SessionTerminate(PEER_FULL, "c-out", WaddleJingleReason.TIMEOUT) in f.client.callVerbs,
        )
        assertEquals(CallState.Ended("c-out", CallEndReason.Timeout), f.store.state.value)
    }

    @Test
    fun `session-accept while outgoing goes active with our initiator identity`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.store.onCallEvent(proceed(PEER_FULL, "c-out"))
        runCurrent()

        f.store.onCallEvent(sessionAccept(PEER_FULL, "c-out"))
        runCurrent()

        assertEquals(
            CallState.Active(peer = PEER_FULL, sid = "c-out", media = audio, join = join, initiator = OWN_FULL),
            f.store.state.value,
        )

        // The armed session-accept timeout must now be a no-op.
        advanceTimeBy(CallStore.SESSION_ACCEPT_TIMEOUT_MILLIS + 1)
        runCurrent()
        assertTrue(f.store.state.value is CallState.Active)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.SessionTerminate })
    }

    @Test
    fun `unanswered outgoing call auto-retracts on the ring timeout`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        advanceTimeBy(CallStore.OUTGOING_TIMEOUT_MILLIS + 1)
        runCurrent()

        // XEP-0353 §5.1.4: retract targets the callee's BARE JID.
        assertTrue(RecordedCallVerb.Retract(PEER_BARE, "c-out") in f.client.callVerbs)
        assertEquals(CallState.Ended("c-out", CallEndReason.Timeout), f.store.state.value)
    }

    @Test
    fun `peer reject ends the outgoing call and cancels the ring timer`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        f.store.onCallEvent(reject(PEER_FULL, "c-out"))
        runCurrent()

        assertEquals(CallState.Ended("c-out", CallEndReason.Rejected), f.store.state.value)
        advanceTimeBy(CallStore.OUTGOING_TIMEOUT_MILLIS + 1)
        runCurrent()
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Retract })
    }

    @Test
    fun `caller retract ends the incoming ring`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        f.store.onCallEvent(retract(PEER_FULL, "c1"))

        assertEquals(CallState.Ended("c1", CallEndReason.Retracted), f.store.state.value)
    }

    // ── Termination ──────────────────────────────────────────────────────────

    @Test
    fun `remote session-terminate ends the active call`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.store.onCallEvent(sessionAccept(PEER_FULL, "c-out"))

        f.store.onCallEvent(sessionTerminate(PEER_FULL, "c-out", WaddleJingleReason.SUCCESS))

        assertEquals(
            CallState.Ended("c-out", CallEndReason.Finished(WaddleJingleReason.SUCCESS)),
            f.store.state.value,
        )
    }

    @Test
    fun `remote finish ends the incoming ring with its reason`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        f.store.onCallEvent(finish(PEER_FULL, "c1", WaddleJingleReason.SUCCESS))

        assertEquals(
            CallState.Ended("c1", CallEndReason.Finished(WaddleJingleReason.SUCCESS)),
            f.store.state.value,
        )
    }

    @Test
    fun `hangUp on an active call terminates then sends the finish bookend`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.store.onCallEvent(sessionAccept(PEER_FULL, "c-out"))
        f.client.callVerbs.clear()

        f.store.hangUp()

        assertEquals(
            listOf<RecordedCallVerb>(
                RecordedCallVerb.SessionTerminateWithOutcome(PEER_FULL, "c-out", WaddleJingleReason.SUCCESS),
                RecordedCallVerb.Finish(PEER_FULL, "c-out"),
            ),
            f.client.callVerbs.toList(),
        )
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `hangUp still sends finish when the terminate is classified orphaned`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.store.onCallEvent(sessionAccept(PEER_FULL, "c-out"))
        f.client.callVerbs.clear()
        f.client.callTerminateOutcome = WaddleCallSessionTerminateOutcome.ORPHANED

        f.store.hangUp()

        assertTrue(RecordedCallVerb.Finish(PEER_FULL, "c-out") in f.client.callVerbs)
    }

    @Test
    fun `hangUp skips the finish bookend when the terminate errored`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.store.onCallEvent(sessionAccept(PEER_FULL, "c-out"))
        f.client.callVerbs.clear()
        f.client.callTerminateOutcome = WaddleCallSessionTerminateOutcome.ERROR

        f.store.hangUp()

        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Finish })
        assertEquals("call session terminate failed", f.store.lastError.value)
    }

    @Test
    fun `hangUp while outgoing retracts the ring`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.client.callVerbs.clear()

        f.store.hangUp()

        assertEquals(listOf<RecordedCallVerb>(RecordedCallVerb.Retract(PEER_BARE, "c-out")), f.client.callVerbs)
        assertEquals(CallState.Idle, f.store.state.value)
    }

    // ── Self-originated carbon filtering ─────────────────────────────────────

    @Test
    fun `own propose carbon never opens an incoming slot or side effects`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        f.store.onCallEvent(propose("$OWN_BARE/other-device", "c1"))
        runCurrent()

        assertEquals(CallState.Idle, f.store.state.value)
        assertTrue(f.client.callVerbs.isEmpty())
    }

    @Test
    fun `own proceed carbon stops the local ring - answered elsewhere`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        f.client.callVerbs.clear()

        f.store.onCallEvent(proceed("$OWN_BARE/other-device", "c1"))
        runCurrent()

        assertEquals(CallState.Idle, f.store.state.value)
        // No side effects for a self-originated carbon: the answering
        // device sends the session-accept, not this one.
        assertTrue(f.client.callVerbs.isEmpty())
    }

    @Test
    fun `own reject carbon stops the local ring - declined elsewhere`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        f.store.onCallEvent(
            WaddleCallEvent(
                from = "$OWN_BARE/other-device",
                to = null,
                sid = "c1",
                kind = WaddleCallEventKind.Reject(reason = null, tieBreak = false),
            ),
        )
        runCurrent()

        assertEquals(CallState.Ended("c1", CallEndReason.Rejected), f.store.state.value)
    }

    @Test
    fun `own carbon with a different sid is ignored`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        f.store.onCallEvent(proceed("$OWN_BARE/other-device", "unrelated"))
        runCurrent()

        assertTrue(f.store.state.value is CallState.Incoming)
    }

    // ── Slot hygiene ─────────────────────────────────────────────────────────

    @Test
    fun `dismiss returns the ended slot to idle`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        f.store.onCallEvent(retract(PEER_FULL, "c1"))

        f.store.dismiss()

        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `a new propose is accepted after the previous call ended`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        f.store.onCallEvent(retract(PEER_FULL, "c1"))

        f.store.onCallEvent(propose(PEER_FULL, "c2"))
        runCurrent()

        assertEquals(CallState.Incoming(from = PEER_FULL, sid = "c2", media = audio), f.store.state.value)
    }
}
