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
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * The call state machine, mirroring the web reducer suite
 * (chat/src/lib/calls tests): propose/proceed/accept/terminate, both
 * XEP-0353 tie-break branches, busy-reject, ringing emission, the
 * auto-retract timeout, and the self-originated-carbon filter — all
 * through the REAL `ClientCallSignaling` wire path into the recording
 * [FakeWaddleClient].
 */
class CallStoreTest {
    private val ownBare = "alice@waddle.test"
    private val ownFull = "alice@waddle.test/waddle-android-1"
    private val peerBare = "bob@waddle.test"
    private val peerFull = "bob@waddle.test/phone"

    private val audio = WaddleCallMedia(audio = true, video = false)
    private val video = WaddleCallMedia(audio = true, video = true)
    private val join = WaddleLiveKitJoin(
        url = "wss://livekit.waddle.test",
        room = "dm-room",
        identity = ownFull,
        token = "jwt",
    )

    private class Fixture(sid: () -> String = { "c-fixed" }) {
        val client = FakeWaddleClient()
        val activeSession = ActiveSession { }
        val store: CallStore

        init {
            activeSession.ownBareJid = "alice@waddle.test"
            activeSession.ownFullJid = "alice@waddle.test/waddle-android-1"
            activeSession.onReady(client)
            store = CallStore(
                signaling = ClientCallSignaling(activeSession),
                ownBareJid = { activeSession.ownBareJid },
                ownFullJid = { activeSession.ownFullJid },
                newSid = sid,
            )
        }
    }

    private fun propose(from: String, sid: String, media: WaddleCallMedia = audio) =
        WaddleCallEvent(from = from, to = null, sid = sid, kind = WaddleCallEventKind.Propose(media))

    private fun proceed(from: String, sid: String) =
        WaddleCallEvent(from = from, to = null, sid = sid, kind = WaddleCallEventKind.Proceed)

    private fun ringing(from: String, sid: String) =
        WaddleCallEvent(from = from, to = null, sid = sid, kind = WaddleCallEventKind.Ringing)

    private fun reject(from: String, sid: String, tieBreak: Boolean = false) = WaddleCallEvent(
        from = from,
        to = null,
        sid = sid,
        kind = WaddleCallEventKind.Reject(
            reason = if (tieBreak) WaddleJingleReason.EXPIRED else null,
            tieBreak = tieBreak,
        ),
    )

    private fun retract(from: String, sid: String) = WaddleCallEvent(
        from = from,
        to = null,
        sid = sid,
        kind = WaddleCallEventKind.Retract(reason = null, tieBreak = false),
    )

    private fun sessionInitiate(from: String, sid: String, media: WaddleCallMedia = audio) =
        WaddleCallEvent(
            from = from,
            to = null,
            sid = sid,
            kind = WaddleCallEventKind.SessionInitiate(join = join, media = media),
        )

    private fun sessionAccept(from: String, sid: String, media: WaddleCallMedia = audio) =
        WaddleCallEvent(
            from = from,
            to = null,
            sid = sid,
            kind = WaddleCallEventKind.SessionAccept(join = join, media = media),
        )

    private fun sessionTerminate(from: String, sid: String, reason: WaddleJingleReason?) =
        WaddleCallEvent(
            from = from,
            to = null,
            sid = sid,
            kind = WaddleCallEventKind.SessionTerminate(reason),
        )

    private fun finish(from: String, sid: String, reason: WaddleJingleReason? = null) =
        WaddleCallEvent(
            from = from,
            to = null,
            sid = sid,
            kind = WaddleCallEventKind.Finish(reason = reason, migratedTo = null),
        )

    // ── Incoming ring ────────────────────────────────────────────────────────

    @Test
    fun `inbound propose surfaces incoming and rings back the caller's bare jid`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        f.store.onCallEvent(propose(peerFull, "c1", video))
        runCurrent()

        val state = f.store.state.value
        assertEquals(CallState.Incoming(from = peerFull, sid = "c1", media = video), state)
        // XEP-0353 §3.2: <ringing/> to the caller's BARE JID.
        assertEquals(listOf<RecordedCallVerb>(RecordedCallVerb.Ringing(peerBare, "c1")), f.client.callVerbs)
    }

    @Test
    fun `duplicate propose for the ringing sid does not ring twice`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        f.store.onCallEvent(propose(peerFull, "c1"))
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        assertEquals(1, f.client.callVerbs.count { it is RecordedCallVerb.Ringing })
    }

    @Test
    fun `accept sends proceed to the proposer's full jid`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        assertTrue(f.store.acceptIncoming())

        assertTrue(RecordedCallVerb.Proceed(peerFull, "c1") in f.client.callVerbs)
        val state = f.store.state.value
        assertTrue(state is CallState.Incoming && state.accepting)
    }

    @Test
    fun `decline sends reject to the proposer's full jid and clears the slot`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        assertTrue(f.store.declineIncoming())

        assertTrue(RecordedCallVerb.Reject(peerFull, "c1") in f.client.callVerbs)
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `session-initiate while incoming goes active and sends session-accept as our resource`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()
        f.store.acceptIncoming()

        f.store.onCallEvent(sessionInitiate(peerFull, "c1"))
        runCurrent()

        val state = f.store.state.value
        assertTrue(state is CallState.Active)
        state as CallState.Active
        assertEquals(peerFull, state.peer)
        assertEquals(join, state.join)
        assertEquals(peerFull, state.initiator)
        // XEP-0166 §6.2: the responder confirms with session-accept;
        // responder attribute is OUR full JID.
        assertTrue(RecordedCallVerb.SessionAccept(peerFull, ownFull, "c1", true, false) in f.client.callVerbs)
    }

    @Test
    fun `stale session-initiate for another sid is dropped`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        f.store.onCallEvent(sessionInitiate(peerFull, "other-sid"))
        runCurrent()

        assertTrue(f.store.state.value is CallState.Incoming)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.SessionAccept })
    }

    // ── Outgoing call ────────────────────────────────────────────────────────

    @Test
    fun `startCall proposes to the bare jid and records the initiator`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)

        assertTrue(f.store.startCall("$peerBare/ignored-resource", audio))

        assertEquals(
            listOf<RecordedCallVerb>(RecordedCallVerb.Propose(peerBare, "c-out", true, false)),
            f.client.callVerbs,
        )
        assertEquals(
            CallState.Outgoing(to = peerBare, sid = "c-out", media = audio, initiator = ownFull),
            f.store.state.value,
        )
    }

    @Test
    fun `startCall is refused while another call is live`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        assertFalse(f.store.startCall("carol@waddle.test", audio))
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Propose })
    }

    @Test
    fun `failed propose rolls the slot back to idle`() = runTest {
        val f = Fixture()
        f.client.callVerbResult = false
        f.store.start(backgroundScope)

        assertFalse(f.store.startCall(peerBare, audio))

        assertEquals(CallState.Idle, f.store.state.value)
        assertEquals("call propose failed", f.store.lastError.value)
    }

    @Test
    fun `ringing from the callee marks the outgoing slot ringing`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)

        f.store.onCallEvent(ringing(peerFull, "c-out"))

        val state = f.store.state.value
        assertTrue(state is CallState.Outgoing && state.ringing)
    }

    @Test
    fun `proceed fires session-initiate to the responder resource and arms the accept timeout`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, video)

        f.store.onCallEvent(proceed(peerFull, "c-out"))
        runCurrent()

        // XEP-0353 §0.6: session-initiate goes to the full JID stamped
        // on the proceed; initiator names our own resource (§7.1).
        assertTrue(
            RecordedCallVerb.SessionInitiate(peerFull, ownFull, "c-out", true, true) in f.client.callVerbs,
        )

        // The accept-gap timeout: no session-accept → terminate + Ended.
        advanceTimeBy(CallStore.SESSION_ACCEPT_TIMEOUT_MILLIS + 1)
        runCurrent()
        assertTrue(
            RecordedCallVerb.SessionTerminate(peerFull, "c-out", WaddleJingleReason.TIMEOUT) in f.client.callVerbs,
        )
        assertEquals(CallState.Ended("c-out", CallEndReason.Timeout), f.store.state.value)
    }

    @Test
    fun `session-accept while outgoing goes active with our initiator identity`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)
        f.store.onCallEvent(proceed(peerFull, "c-out"))
        runCurrent()

        f.store.onCallEvent(sessionAccept(peerFull, "c-out"))
        runCurrent()

        assertEquals(
            CallState.Active(peer = peerFull, sid = "c-out", media = audio, join = join, initiator = ownFull),
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
        f.store.startCall(peerBare, audio)

        advanceTimeBy(CallStore.OUTGOING_TIMEOUT_MILLIS + 1)
        runCurrent()

        // XEP-0353 §5.1.4: retract targets the callee's BARE JID.
        assertTrue(RecordedCallVerb.Retract(peerBare, "c-out") in f.client.callVerbs)
        assertEquals(CallState.Ended("c-out", CallEndReason.Timeout), f.store.state.value)
    }

    @Test
    fun `peer reject ends the outgoing call and cancels the ring timer`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)

        f.store.onCallEvent(reject(peerFull, "c-out"))
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
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        f.store.onCallEvent(retract(peerFull, "c1"))

        assertEquals(CallState.Ended("c1", CallEndReason.Retracted), f.store.state.value)
    }

    // ── Busy + tie-breaks ────────────────────────────────────────────────────

    @Test
    fun `propose from an unrelated caller while ringing is busy-rejected`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
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
        f.store.startCall(peerBare, audio)

        // "c-aa" < "c-zz" under i;octet → the incoming propose wins.
        f.store.onCallEvent(propose(peerFull, "c-aa"))
        runCurrent()

        // XEP-0353 tie-break-1: retract OUR higher sid with
        // <tie-break/> + <expired/>, then treat theirs as a normal ring.
        assertTrue(RecordedCallVerb.RetractTieBreak(peerFull, "c-zz") in f.client.callVerbs)
        assertEquals(
            CallState.Incoming(from = peerFull, sid = "c-aa", media = audio),
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
        f.store.startCall(peerBare, audio)

        f.store.onCallEvent(propose(peerFull, "c-zz"))
        // runCurrent, not advanceUntilIdle: the outgoing ring timer must
        // stay armed — advancing virtual time to idle would fire it.
        runCurrent()

        // XEP-0353 tie-break-1: reject THEIR higher sid with
        // <tie-break/> + <expired/>; our outgoing ring survives.
        assertTrue(RecordedCallVerb.RejectTieBreak(peerFull, "c-zz") in f.client.callVerbs)
        val state = f.store.state.value
        assertTrue(state is CallState.Outgoing && state.sid == "c-aa")
    }

    @Test
    fun `tie-break reject with expired ends our outgoing slot as expired`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)

        f.store.onCallEvent(reject(peerFull, "c-out", tieBreak = true))

        assertEquals(CallState.Ended("c-out", CallEndReason.Expired), f.store.state.value)
    }

    @Test
    fun `re-propose from the active peer migrates the session`() = runTest {
        val f = Fixture(sid = { "c-old" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)
        f.store.onCallEvent(proceed(peerFull, "c-old"))
        f.store.onCallEvent(sessionAccept(peerFull, "c-old"))
        runCurrent()
        f.client.callVerbs.clear()

        f.store.onCallEvent(propose(peerFull, "c-new"))
        runCurrent()

        // finish(migrated) + proceed ordering matters: the migration
        // markers must be on the wire before the new ring is accepted.
        val kinds = f.client.callVerbs.toList()
        assertEquals(RecordedCallVerb.FinishMigrated(peerFull, "c-old", "c-new"), kinds[0])
        assertEquals(RecordedCallVerb.Proceed(peerFull, "c-new"), kinds[1])
        assertTrue(
            RecordedCallVerb.SessionTerminate(peerFull, "c-old", WaddleJingleReason.EXPIRED) in kinds,
        )
        // accepting = true: the <proceed/> is already out, so the
        // migrated ring must not re-notify or offer a duplicate Accept.
        assertEquals(
            CallState.Incoming(from = peerFull, sid = "c-new", media = audio, accepting = true),
            f.store.state.value,
        )
    }

    @Test
    fun `accepted ring times out when the caller never session-initiates`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
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
            RecordedCallVerb.FinishWithReason(peerFull, "c1", WaddleJingleReason.TIMEOUT) in
                f.client.callVerbs,
        )
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
    }

    @Test
    fun `session-initiate retires the responder's accept timer`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()
        assertTrue(f.store.acceptIncoming())
        f.store.onCallEvent(sessionInitiate(peerFull, "c1"))
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
        f.store.startCall(peerBare, audio)
        f.store.onCallEvent(proceed(peerFull, "c-old"))
        f.store.onCallEvent(sessionAccept(peerFull, "c-old"))
        runCurrent()
        f.store.onCallEvent(propose(peerFull, "c-new"))
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
            RecordedCallVerb.FinishWithReason(peerFull, "c-new", WaddleJingleReason.TIMEOUT) in
                f.client.callVerbs,
        )
    }

    @Test
    fun `losing the tie-break on the wire still rings the winning propose`() = runTest {
        val f = Fixture(sid = { "c-zz" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)

        // Peer's winning (lower-sid) propose is queued behind the
        // effect channel; before it runs, the peer's tie-break reject
        // of OUR sid lands and the reducer retires the slot as
        // Ended(Expired). The queued effect must then treat the peer's
        // propose as the tie-break WINNER — ring it — not decline it
        // (which would kill both calls).
        f.store.onCallEvent(propose(peerFull, "c-aa"))
        f.store.onCallEvent(reject(peerFull, "c-zz", tieBreak = true))
        runCurrent()

        assertEquals(
            CallState.Incoming(from = peerFull, sid = "c-aa", media = audio),
            f.store.state.value,
        )
        assertTrue(RecordedCallVerb.Ringing(peerBare, "c-aa") in f.client.callVerbs)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
    }

    @Test
    fun `declining an accepting ring finishes with cancel instead of a late reject`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()
        assertTrue(f.store.acceptIncoming())
        f.client.callVerbs.clear()

        assertTrue(f.store.declineIncoming())

        assertTrue(
            RecordedCallVerb.FinishWithReason(peerFull, "c1", WaddleJingleReason.CANCEL) in
                f.client.callVerbs,
        )
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.Reject })
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `tie-break effect racing a local hang-up still declines the peer's propose`() = runTest {
        val f = Fixture(sid = { "c-zz" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)

        // The winning (lower-sid) propose is queued behind the effect
        // channel; the user hangs up before it runs.
        f.store.onCallEvent(propose(peerFull, "c-aa"))
        f.store.hangUp()
        runCurrent()

        // Our sid was retracted by the hang-up; the peer's propose must
        // still get an answer instead of ringing to their timeout —
        // and the dead tie-break must not resurrect a ring.
        assertTrue(RecordedCallVerb.Retract(peerBare, "c-zz") in f.client.callVerbs)
        assertTrue(RecordedCallVerb.Reject(peerFull, "c-aa") in f.client.callVerbs)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.RetractTieBreak })
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `stale proceed effect cannot touch a newer call`() = runTest {
        var callIndex = 0
        val f = Fixture(sid = { if (callIndex++ == 0) "c-first" else "c-second" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)

        // Peer proceed for c-first is queued; before it runs the user
        // hangs up and dials again — the slot now belongs to c-second.
        f.store.onCallEvent(proceed(peerFull, "c-first"))
        f.store.hangUp()
        f.store.startCall(peerBare, audio)
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
        f.store.startCall(peerBare, audio)
        f.activeSession.ownFullJid = null

        // "c-aa" < "c-zz": the incoming propose wins the tie-break, so
        // OUR sid must be retracted (XEP-0353 tie-break-1) even without
        // a bound resource; theirs is declined since we cannot host.
        f.store.onCallEvent(propose(peerFull, "c-aa"))
        runCurrent()

        assertTrue(RecordedCallVerb.RetractTieBreak(peerFull, "c-zz") in f.client.callVerbs)
        assertTrue(RecordedCallVerb.Reject(peerFull, "c-aa") in f.client.callVerbs)
        // The retracted sid must not keep ringing locally.
        assertEquals(CallState.Ended("c-zz", CallEndReason.Expired), f.store.state.value)
    }

    @Test
    fun `tie-break without a bound resource still rejects the losing incoming sid`() = runTest {
        val f = Fixture(sid = { "c-aa" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)
        f.activeSession.ownFullJid = null

        f.store.onCallEvent(propose(peerFull, "c-zz"))
        runCurrent()

        assertTrue(RecordedCallVerb.RejectTieBreak(peerFull, "c-zz") in f.client.callVerbs)
        assertTrue(f.client.callVerbs.none { it is RecordedCallVerb.RetractTieBreak })
    }

    // ── Termination ──────────────────────────────────────────────────────────

    @Test
    fun `remote session-terminate ends the active call`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)
        f.store.onCallEvent(sessionAccept(peerFull, "c-out"))

        f.store.onCallEvent(sessionTerminate(peerFull, "c-out", WaddleJingleReason.SUCCESS))

        assertEquals(
            CallState.Ended("c-out", CallEndReason.Finished(WaddleJingleReason.SUCCESS)),
            f.store.state.value,
        )
    }

    @Test
    fun `remote finish ends the incoming ring with its reason`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        f.store.onCallEvent(finish(peerFull, "c1", WaddleJingleReason.SUCCESS))

        assertEquals(
            CallState.Ended("c1", CallEndReason.Finished(WaddleJingleReason.SUCCESS)),
            f.store.state.value,
        )
    }

    @Test
    fun `hangUp on an active call terminates then sends the finish bookend`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)
        f.store.onCallEvent(sessionAccept(peerFull, "c-out"))
        f.client.callVerbs.clear()

        f.store.hangUp()

        assertEquals(
            listOf<RecordedCallVerb>(
                RecordedCallVerb.SessionTerminateWithOutcome(peerFull, "c-out", WaddleJingleReason.SUCCESS),
                RecordedCallVerb.Finish(peerFull, "c-out"),
            ),
            f.client.callVerbs.toList(),
        )
        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `hangUp still sends finish when the terminate is classified orphaned`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)
        f.store.onCallEvent(sessionAccept(peerFull, "c-out"))
        f.client.callVerbs.clear()
        f.client.callTerminateOutcome = WaddleCallSessionTerminateOutcome.ORPHANED

        f.store.hangUp()

        assertTrue(RecordedCallVerb.Finish(peerFull, "c-out") in f.client.callVerbs)
    }

    @Test
    fun `hangUp skips the finish bookend when the terminate errored`() = runTest {
        val f = Fixture(sid = { "c-out" })
        f.store.start(backgroundScope)
        f.store.startCall(peerBare, audio)
        f.store.onCallEvent(sessionAccept(peerFull, "c-out"))
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
        f.store.startCall(peerBare, audio)
        f.client.callVerbs.clear()

        f.store.hangUp()

        assertEquals(listOf<RecordedCallVerb>(RecordedCallVerb.Retract(peerBare, "c-out")), f.client.callVerbs)
        assertEquals(CallState.Idle, f.store.state.value)
    }

    // ── Self-originated carbon filtering ─────────────────────────────────────

    @Test
    fun `own propose carbon never opens an incoming slot or side effects`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)

        f.store.onCallEvent(propose("$ownBare/other-device", "c1"))
        runCurrent()

        assertEquals(CallState.Idle, f.store.state.value)
        assertTrue(f.client.callVerbs.isEmpty())
    }

    @Test
    fun `own proceed carbon stops the local ring - answered elsewhere`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()
        f.client.callVerbs.clear()

        f.store.onCallEvent(proceed("$ownBare/other-device", "c1"))
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
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        f.store.onCallEvent(
            WaddleCallEvent(
                from = "$ownBare/other-device",
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
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()

        f.store.onCallEvent(proceed("$ownBare/other-device", "unrelated"))
        runCurrent()

        assertTrue(f.store.state.value is CallState.Incoming)
    }

    // ── Slot hygiene ─────────────────────────────────────────────────────────

    @Test
    fun `dismiss returns the ended slot to idle`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()
        f.store.onCallEvent(retract(peerFull, "c1"))

        f.store.dismiss()

        assertEquals(CallState.Idle, f.store.state.value)
    }

    @Test
    fun `a new propose is accepted after the previous call ended`() = runTest {
        val f = Fixture()
        f.store.start(backgroundScope)
        f.store.onCallEvent(propose(peerFull, "c1"))
        runCurrent()
        f.store.onCallEvent(retract(peerFull, "c1"))

        f.store.onCallEvent(propose(peerFull, "c2"))
        runCurrent()

        assertEquals(CallState.Incoming(from = peerFull, sid = "c2", media = audio), f.store.state.value)
    }
}
