package social.waddle.android.client.calls

import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.RecordedCallVerb
import social.waddle.client.ffi.WaddleJingleReason

/**
 * XEP-0353 tie-breaking, session migration, responder timers, and the
 * queued-effect race guards — the CallStore behaviors where the
 * reducer can outpace the effects queue (the web never can: its
 * effects run inline). Core reducer/action coverage lives in
 * [CallStoreTest].
 */
class CallStoreTieBreakTest {
    // ── Busy + tie-breaks ────────────────────────────────────────────────────

    @Test
    fun `propose from an unrelated caller while ringing is busy-rejected`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()

        f.store.onCallEvent(propose("carol@waddle.test/tab", "c2"))
        runCurrent()

        // Slot untouched; the second caller gets a plain reject to the
        // full JID so they stop ringing (XEP-0353 has no <busy/>).
        val state = f.store.state.value
        assertTrue(state is CallState.Incoming && state.sid == "c1")
        assertTrue(RecordedCallVerb.Reject("carol@waddle.test/tab", "c2") in f.client.callVerbs)
    }

    @Test
    fun `tie-break where the incoming sid is lower retracts ours and takes theirs`() = runTest {
        val f = Fixture(sid = { "c-zz" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        // "c-aa" < "c-zz" under i;octet → the incoming propose wins.
        f.store.onCallEvent(propose(PEER_FULL, "c-aa"))
        runCurrent()

        // XEP-0353 tie-break-1: retract OUR higher sid with
        // <tie-break/> + <expired/>, then treat theirs as a normal ring.
        assertTrue(RecordedCallVerb.RetractTieBreak(PEER_FULL, "c-zz") in f.client.callVerbs)
        assertEquals(
            CallState.Incoming(from = PEER_FULL, sid = "c-aa", media = audio),
            f.store.state.value,
        )
        // Our auto-retract timer must be dead: the slot is theirs now.
        advanceTimeBy(CallStore.OUTGOING_TIMEOUT_MILLIS + 1)
        runCurrent()
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Retract })
    }

    @Test
    fun `tie-break where the incoming sid is higher rejects theirs and keeps ringing`() = runTest {
        val f = Fixture(sid = { "c-aa" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        f.store.onCallEvent(propose(PEER_FULL, "c-zz"))
        // runCurrent, not advanceUntilIdle: the outgoing ring timer must
        // stay armed — advancing virtual time to idle would fire it.
        runCurrent()

        // XEP-0353 tie-break-1: reject THEIR higher sid with
        // <tie-break/> + <expired/>; our outgoing ring survives.
        assertTrue(RecordedCallVerb.RejectTieBreak(PEER_FULL, "c-zz") in f.client.callVerbs)
        val state = f.store.state.value
        assertTrue(state is CallState.Outgoing && state.sid == "c-aa")
    }

    @Test
    fun `tie-break reject with expired ends our outgoing slot as expired`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        f.store.onCallEvent(reject(PEER_FULL, "c-out", tieBreak = true))

        assertEquals(CallState.Ended("c-out", CallEndReason.Expired), f.store.state.value)
    }

    @Test
    fun `re-propose from the active peer migrates the session`() = runTest {
        val f = Fixture(sid = { "c-old" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.store.onCallEvent(proceed(PEER_FULL, "c-old"))
        f.store.onCallEvent(sessionAccept(PEER_FULL, "c-old"))
        runCurrent()
        f.client.callVerbs.clear()

        f.store.onCallEvent(propose(PEER_FULL, "c-new"))
        runCurrent()

        // finish(migrated) + proceed ordering matters: the migration
        // markers must be on the wire before the new ring is accepted.
        val kinds = f.client.callVerbs.toList()
        assertEquals(RecordedCallVerb.FinishMigrated(PEER_FULL, "c-old", "c-new"), kinds[0])
        assertEquals(RecordedCallVerb.Proceed(PEER_FULL, "c-new"), kinds[1])
        assertTrue(
            RecordedCallVerb.SessionTerminate(PEER_FULL, "c-old", WaddleJingleReason.EXPIRED) in kinds,
        )
        // accepting = true: the <proceed/> is already out, so the
        // migrated ring must not re-notify or offer a duplicate Accept.
        assertEquals(
            CallState.Incoming(from = PEER_FULL, sid = "c-new", media = audio, accepting = true),
            f.store.state.value,
        )
    }

    @Test
    fun `accepted ring times out when the caller never session-initiates`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        assertTrue(f.store.acceptIncoming())
        f.client.callVerbs.clear()

        advanceTimeBy(CallStore.SESSION_ACCEPT_TIMEOUT_MILLIS + 1)
        runCurrent()

        // Our <proceed/> went out but the caller died before the Jingle
        // session-initiate: the accepting slot (which pins a foreground
        // service on Android) must be bounded — and the abandon verb is
        // the <finish/> bookend (we already answered the propose), not
        // a contradictory late reject.
        assertEquals(CallState.Ended("c1", CallEndReason.Timeout), f.store.state.value)
        assertTrue(
            RecordedCallVerb.FinishWithReason(PEER_FULL, "c1", WaddleJingleReason.TIMEOUT) in
                f.client.callVerbs,
        )
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
    }

    @Test
    fun `session-initiate retires the responder's accept timer`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        assertTrue(f.store.acceptIncoming())
        f.store.onCallEvent(sessionInitiate(PEER_FULL, "c1"))
        runCurrent()
        f.client.callVerbs.clear()

        advanceTimeBy(CallStore.SESSION_ACCEPT_TIMEOUT_MILLIS + 1)
        runCurrent()

        val state = f.store.state.value
        assertTrue(state is CallState.Active && state.sid == "c1")
        assertTrue(f.client.callVerbs.isEmpty())
    }

    @Test
    fun `migrated ring times out when the caller never session-initiates`() = runTest {
        val f = Fixture(sid = { "c-old" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.store.onCallEvent(proceed(PEER_FULL, "c-old"))
        f.store.onCallEvent(sessionAccept(PEER_FULL, "c-old"))
        runCurrent()
        f.store.onCallEvent(propose(PEER_FULL, "c-new"))
        runCurrent()
        f.client.callVerbs.clear()

        advanceTimeBy(CallStore.SESSION_ACCEPT_TIMEOUT_MILLIS + 1)
        runCurrent()

        // We formally accepted c-new with the migration <proceed/>; a
        // caller that dies before session-initiate must not pin the
        // accepting slot (and its foreground service) forever, and the
        // abandon verb is the <finish/> bookend.
        assertEquals(CallState.Ended("c-new", CallEndReason.Timeout), f.store.state.value)
        assertTrue(
            RecordedCallVerb.FinishWithReason(PEER_FULL, "c-new", WaddleJingleReason.TIMEOUT) in
                f.client.callVerbs,
        )
    }

    @Test
    fun `losing the tie-break on the wire still rings the winning propose`() = runTest {
        val f = Fixture(sid = { "c-zz" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        // Peer's winning (lower-sid) propose is queued behind the
        // effect channel; before it runs, the peer's tie-break reject
        // of OUR sid lands and the reducer retires the slot as
        // Ended(Expired). The queued effect must then treat the peer's
        // propose as the tie-break WINNER — ring it — not decline it
        // (which would kill both calls).
        f.store.onCallEvent(propose(PEER_FULL, "c-aa"))
        f.store.onCallEvent(reject(PEER_FULL, "c-zz", tieBreak = true))
        runCurrent()

        assertEquals(
            CallState.Incoming(from = PEER_FULL, sid = "c-aa", media = audio),
            f.store.state.value,
        )
        assertTrue(RecordedCallVerb.Ringing(PEER_BARE, "c-aa") in f.client.callVerbs)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
    }

    @Test
    fun `losing the tie-break does not ring a propose the peer already retracted`() = runTest {
        val f = Fixture(sid = { "c-zz" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        // Stanza burst: winning propose queued, tie-break reject of our
        // sid, then the peer retracts their own propose — all reduced
        // before the queued effect runs. The retract matches no slot
        // (ours is c-zz), so only the aborted-sid memory can stop the
        // effect from resurrecting c-aa as an unbounded ghost ring.
        f.store.onCallEvent(propose(PEER_FULL, "c-aa"))
        f.store.onCallEvent(reject(PEER_FULL, "c-zz", tieBreak = true))
        f.store.onCallEvent(retract(PEER_FULL, "c-aa"))
        runCurrent()

        assertEquals(CallState.Ended("c-zz", CallEndReason.Expired), f.store.state.value)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Ringing })
    }

    @Test
    fun `a retract landing mid-tie-break does not install the dead ring`() = runTest {
        val f = Fixture(sid = { "c-zz" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        // The winning propose passes the entry guard and the effect
        // suspends inside the tie-break retract send; the peer's
        // retract of their own propose reduces meanwhile (matching no
        // slot — ours is still c-zz — so it never re-applies). The
        // resumed effect must not install the retracted ring, and our
        // retracted sid must end locally instead of ringing on.
        f.client.callRetractTieBreakDelayMillis = 1_000
        f.store.onCallEvent(propose(PEER_FULL, "c-aa"))
        runCurrent()
        f.store.onCallEvent(retract(PEER_FULL, "c-aa"))
        advanceTimeBy(1_001)
        runCurrent()

        assertEquals(CallState.Ended("c-zz", CallEndReason.Expired), f.store.state.value)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Ringing })
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
    }

    @Test
    fun `hanging up an accepting ring finishes with cancel instead of a late reject`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        assertTrue(f.store.acceptIncoming())
        f.client.callVerbs.clear()

        f.store.hangUp()

        assertTrue(
            RecordedCallVerb.FinishWithReason(PEER_FULL, "c1", WaddleJingleReason.CANCEL) in
                f.client.callVerbs,
        )
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `migration abort after a concurrent hang-up finishes the proceeded sid with cancel`() = runTest {
        val f = Fixture(sid = { "c-old" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.store.onCallEvent(proceed(PEER_FULL, "c-old"))
        f.store.onCallEvent(sessionAccept(PEER_FULL, "c-old"))
        runCurrent()

        // Stall the migration <proceed/> so the user's hang-up lands
        // between the wire sends and the slot swap.
        f.client.callProceedDelayMillis = 1_000
        f.store.onCallEvent(propose(PEER_FULL, "c-new"))
        runCurrent()
        f.store.hangUp()
        f.client.callVerbs.clear()
        advanceTimeBy(1_001)
        runCurrent()

        // The migration proceed for c-new is on the wire but the slot
        // belongs to the hang-up now: the proceeded sid is abandoned
        // with the finish bookend, never a contradictory late reject.
        assertTrue(
            RecordedCallVerb.FinishWithReason(PEER_FULL, "c-new", WaddleJingleReason.CANCEL) in
                f.client.callVerbs,
        )
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `declining an accepting ring finishes with cancel instead of a late reject`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(PEER_FULL, "c1"))
        runCurrent()
        assertTrue(f.store.acceptIncoming())
        f.client.callVerbs.clear()

        assertTrue(f.store.declineIncoming())

        assertTrue(
            RecordedCallVerb.FinishWithReason(PEER_FULL, "c1", WaddleJingleReason.CANCEL) in
                f.client.callVerbs,
        )
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `tie-break effect racing a local hang-up still declines the peer's propose`() = runTest {
        val f = Fixture(sid = { "c-zz" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        // The winning (lower-sid) propose is queued behind the effect
        // channel; the user hangs up before it runs.
        f.store.onCallEvent(propose(PEER_FULL, "c-aa"))
        f.store.hangUp()
        runCurrent()

        // Our sid was retracted by the hang-up; the peer's propose must
        // still get an answer instead of ringing to their timeout —
        // and the dead tie-break must not resurrect a ring.
        assertTrue(RecordedCallVerb.Retract(PEER_BARE, "c-zz") in f.client.callVerbs)
        assertTrue(RecordedCallVerb.Reject(PEER_FULL, "c-aa") in f.client.callVerbs)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.RetractTieBreak })
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `stale proceed effect cannot touch a newer call`() = runTest {
        var callIndex = 0
        val f = Fixture(sid = { if (callIndex++ == 0) "c-first" else "c-second" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)

        // Peer proceed for c-first is queued; before it runs the user
        // hangs up and dials again — the slot now belongs to c-second.
        f.store.onCallEvent(proceed(PEER_FULL, "c-first"))
        f.store.hangUp()
        f.store.startCall(PEER_BARE, audio)
        runCurrent()

        // No session-initiate may go out for the retracted c-first, and
        // the stale effect must not clobber the live c-second slot.
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.SessionInitiate })
        val state = f.store.state.value
        assertTrue(state is CallState.Outgoing && state.sid == "c-second")
    }

    @Test
    fun `tie-break without a bound resource retracts the losing sid and declines the survivor`() = runTest {
        val f = Fixture(sid = { "c-zz" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.activeSession.ownFullJid = null

        // "c-aa" < "c-zz": the incoming propose wins the tie-break, so
        // OUR sid must be retracted (XEP-0353 tie-break-1) even without
        // a bound resource; theirs is declined since we cannot host.
        f.store.onCallEvent(propose(PEER_FULL, "c-aa"))
        runCurrent()

        assertTrue(RecordedCallVerb.RetractTieBreak(PEER_FULL, "c-zz") in f.client.callVerbs)
        assertTrue(RecordedCallVerb.Reject(PEER_FULL, "c-aa") in f.client.callVerbs)
        // The retracted sid must not keep ringing locally.
        assertEquals(CallState.Ended("c-zz", CallEndReason.Expired), f.store.state.value)
    }

    @Test
    fun `tie-break without a bound resource still rejects the losing incoming sid`() = runTest {
        val f = Fixture(sid = { "c-aa" })
        f.store.start(backgroundScope)
        f.store.startCall(PEER_BARE, audio)
        f.activeSession.ownFullJid = null

        f.store.onCallEvent(propose(PEER_FULL, "c-zz"))
        runCurrent()

        assertTrue(RecordedCallVerb.RejectTieBreak(PEER_FULL, "c-zz") in f.client.callVerbs)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.RetractTieBreak })
    }
}
