package social.waddle.android.feature.call

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.R
import social.waddle.android.client.calls.CallEndReason
import social.waddle.android.client.calls.CallState
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleJingleReason
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * Speaking indicators: LiveKit identities (full JIDs) map to bare JIDs,
 * a speaker lights at once, and goes dark only after the release hold so
 * the ember ring does not flicker between words.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class CallSpeakersTest {
    @Test
    fun identitiesMapToNormalisedBareJids() {
        assertEquals(
            setOf("bob@waddle.test", "david@waddle.test"),
            speakingBareJidsOf(listOf("Bob@Waddle.test/phone", "david@waddle.test/waddle-android-1")),
        )
    }

    @Test
    fun twoDevicesOfOneAccountAreOneSpeaker() {
        assertEquals(
            setOf("bob@waddle.test"),
            speakingBareJidsOf(listOf("bob@waddle.test/phone", "bob@waddle.test/laptop")),
        )
    }

    @Test
    fun identitiesThatAreNotJidsAreDropped() {
        assertEquals(emptySet<String>(), speakingBareJidsOf(listOf("", "  ", "egress-recorder")))
    }

    @Test
    fun aSpeakerLightsImmediately() = runTest {
        val upstream = MutableStateFlow(emptySet<String>())
        val seen = mutableListOf<Set<String>>()
        val job = launch { upstream.releasingAfter(SPEAKING_RELEASE_MILLIS).toList(seen) }
        runCurrent()
        upstream.value = setOf("bob@waddle.test")
        runCurrent()
        assertEquals(setOf("bob@waddle.test"), seen.last())
        job.cancel()
    }

    @Test
    fun aSpeakerIsReleasedOnlyAfterTheHold() = runTest {
        val upstream = MutableStateFlow(setOf("bob@waddle.test"))
        val seen = mutableListOf<Set<String>>()
        val job = launch { upstream.releasingAfter(SPEAKING_RELEASE_MILLIS).toList(seen) }
        runCurrent()
        upstream.value = emptySet()
        advanceTimeBy(SPEAKING_RELEASE_MILLIS - 1)
        runCurrent()
        assertEquals(setOf("bob@waddle.test"), seen.last())
        advanceTimeBy(2)
        runCurrent()
        assertEquals(emptySet<String>(), seen.last())
        job.cancel()
    }

    @Test
    fun aPauseShorterThanTheHoldDoesNotFlicker() = runTest {
        val upstream = MutableStateFlow(setOf("bob@waddle.test"))
        val seen = mutableListOf<Set<String>>()
        val job = launch { upstream.releasingAfter(SPEAKING_RELEASE_MILLIS).toList(seen) }
        runCurrent()
        upstream.value = emptySet()
        advanceTimeBy(SPEAKING_RELEASE_MILLIS / 2)
        runCurrent()
        upstream.value = setOf("bob@waddle.test")
        runCurrent()
        advanceTimeBy(SPEAKING_RELEASE_MILLIS * 2)
        runCurrent()
        // Bob never went dark: one emission, still lit.
        assertEquals(listOf(setOf("bob@waddle.test")), seen)
        job.cancel()
    }

    @Test
    fun eachSpeakerIsReleasedOnTheirOwnClock() = runTest {
        val upstream = MutableStateFlow(setOf("bob@waddle.test", "mara@waddle.test"))
        val seen = mutableListOf<Set<String>>()
        val job = launch { upstream.releasingAfter(SPEAKING_RELEASE_MILLIS).toList(seen) }
        runCurrent()
        upstream.value = setOf("mara@waddle.test")
        advanceTimeBy(SPEAKING_RELEASE_MILLIS / 2)
        runCurrent()
        upstream.value = emptySet()
        runCurrent()
        // Bob's hold ends first; mara's started later.
        advanceTimeBy(SPEAKING_RELEASE_MILLIS / 2 + 1)
        runCurrent()
        assertEquals(setOf("mara@waddle.test"), seen.last())
        advanceTimeBy(SPEAKING_RELEASE_MILLIS / 2)
        runCurrent()
        assertEquals(emptySet<String>(), seen.last())
        job.cancel()
    }

    @Test
    fun rosterLightsSpeakersAndPutsSelfFirst() {
        val roster = listOf(
            MucRosterEntry(nick = "bob", handRaised = false, muted = false, jid = "bob@waddle.test"),
            MucRosterEntry(nick = "ghost", handRaised = false, muted = false, jid = null),
            MucRosterEntry(nick = "david", handRaised = false, muted = true, jid = "david@waddle.test"),
        )
        val voiced = rosterWithVoices(
            roster,
            speakingJids = setOf("bob@waddle.test"),
            selfBareJid = "David@waddle.test/waddle-android-1",
        )
        assertEquals(listOf("david", "bob", "ghost"), voiced.map { it.nick })
        assertTrue(voiced[0].self)
        assertTrue(voiced[0].muted)
        assertFalse(voiced[0].speaking)
        assertTrue(voiced[1].speaking)
        // A row without a resolved JID can neither speak nor be us.
        assertFalse(voiced[2].speaking || voiced[2].self)
    }

    @Test
    fun phaseFollowsTheSlotAndTheMediaTransport() {
        val join = WaddleLiveKitJoin(url = "wss://lk", room = "r", identity = "david@waddle.test/a", token = "t")
        val media = WaddleCallMedia(audio = true, video = false)
        val active = CallState.Active(peer = "bob@waddle.test/phone", sid = "s", media = media, join = join)
        assertEquals(
            CallPhase.Ringing,
            callPhaseOf(CallState.Outgoing("bob@waddle.test", "s", media, ringing = true), 0, CallMediaConnection.Idle),
        )
        assertEquals(CallPhase.Connecting, callPhaseOf(active, 0, CallMediaConnection.Connecting))
        assertEquals(CallPhase.Live(42), callPhaseOf(active, 42, CallMediaConnection.Connected))
        assertEquals(CallPhase.Reconnecting(42), callPhaseOf(active, 42, CallMediaConnection.Reconnecting))
        assertEquals(null, callPhaseOf(CallState.Idle, 0, CallMediaConnection.Idle))
    }

    @Test
    fun everyEndReasonHasWords() {
        assertEquals(R_DECLINED, callEndedReasonRes(CallEndReason.Rejected))
        assertEquals(R_DECLINED, callEndedReasonRes(CallEndReason.Finished(WaddleJingleReason.DECLINE)))
        assertEquals(
            callEndedReasonRes(CallEndReason.Finished(WaddleJingleReason.CONNECTIVITY_ERROR)),
            callEndedReasonRes(CallEndReason.Finished(WaddleJingleReason.GONE)),
        )
        assertEquals(callEndedReasonRes(null), callEndedReasonRes(CallEndReason.Finished(WaddleJingleReason.SUCCESS)))
    }

    private companion object {
        val R_DECLINED = R.string.call_ended_declined
    }
}
