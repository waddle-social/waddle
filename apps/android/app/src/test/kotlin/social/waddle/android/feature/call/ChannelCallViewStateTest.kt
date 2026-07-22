package social.waddle.android.feature.call

import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * Channel top-bar controls + conversation banner state (the web
 * MucCallButton / ConversationCallBanner group semantics).
 */
class ChannelCallViewStateTest {
    private val room = "room@muc.waddle.test"
    private val audio = WaddleCallMedia(audio = true, video = false)

    private fun activeMucCall(peer: String = room) = CallState.Active(
        peer = peer,
        sid = "sid-1",
        media = audio,
        join = WaddleLiveKitJoin(
            url = "wss://livekit.waddle.test",
            room = peer,
            identity = "me@waddle.test/android",
            token = "jwt",
        ),
        kind = CallKind.MUC,
        selfNick = "me",
    )

    private fun activeDmCall() = CallState.Active(
        peer = "bob@waddle.test/phone",
        sid = "sid-dm",
        media = audio,
        join = WaddleLiveKitJoin(
            url = "wss://livekit.waddle.test",
            room = "dm-room",
            identity = "me@waddle.test/android",
            token = "jwt",
        ),
    )

    // ── Top bar ──────────────────────────────────────────────────────────────

    @Test
    fun idleSlotAndNoRoomCallShowsTheStartButtons() {
        assertEquals(
            ChannelCallControls.Start,
            channelCallControlsOf(CallState.Idle, room, emptyList(), videoCall = false),
        )
    }

    @Test
    fun endedSlotStillCountsAsFree() {
        assertEquals(
            ChannelCallControls.Start,
            channelCallControlsOf(
                CallState.Ended(sid = "s", reason = null), room, emptyList(), videoCall = false,
            ),
        )
    }

    @Test
    fun liveRoomCallTurnsTheButtonsIntoAJoinAffordance() {
        assertEquals(
            ChannelCallControls.Join(participantCount = 2, video = true),
            channelCallControlsOf(CallState.Idle, room, listOf("alice", "bob"), videoCall = true),
        )
    }

    @Test
    fun busyWithACallElsewhereHidesTheControls() {
        assertEquals(
            ChannelCallControls.Hidden,
            channelCallControlsOf(activeDmCall(), room, listOf("alice"), videoCall = false),
        )
        assertEquals(
            ChannelCallControls.Hidden,
            channelCallControlsOf(
                CallState.Outgoing(to = "bob@waddle.test", sid = "s", media = audio),
                room,
                emptyList(),
                videoCall = false,
            ),
        )
    }

    @Test
    fun ourOwnCallInThisRoomHidesTheControls() {
        assertEquals(
            ChannelCallControls.Hidden,
            channelCallControlsOf(activeMucCall(), room, listOf("me"), videoCall = false),
        )
        assertEquals(
            ChannelCallControls.Hidden,
            channelCallControlsOf(
                CallState.MucPending(room, "sid", audio, selfNick = "me", selfFullJid = null),
                room,
                emptyList(),
                videoCall = false,
            ),
        )
    }

    @Test
    fun roomJidComparisonIsCaseInsensitive() {
        assertEquals(
            ChannelCallControls.Hidden,
            channelCallControlsOf(activeMucCall(), "Room@MUC.Waddle.Test", listOf("me"), videoCall = false),
        )
    }

    // ── Banner ───────────────────────────────────────────────────────────────

    @Test
    fun noCallMeansNoBanner() {
        assertEquals(
            ChannelCallBannerState.Hidden,
            channelCallBannerOf(CallState.Idle, room, emptyList(), videoCall = false),
        )
    }

    @Test
    fun liveCallWeAreNotInShowsTheJoinBanner() {
        assertEquals(
            ChannelCallBannerState.Join(
                participantCount = 2,
                nicks = listOf("alice", "bob"),
                video = false,
                busy = false,
            ),
            channelCallBannerOf(CallState.Idle, room, listOf("alice", "bob"), videoCall = false),
        )
    }

    @Test
    fun busySlotDisablesTheBannerJoin() {
        assertEquals(
            ChannelCallBannerState.Join(
                participantCount = 1,
                nicks = listOf("alice"),
                video = false,
                busy = true,
            ),
            channelCallBannerOf(activeDmCall(), room, listOf("alice"), videoCall = false),
        )
    }

    @Test
    fun ourOwnRoomCallShowsTheCompactOngoingPill() {
        assertEquals(
            ChannelCallBannerState.Ongoing(participantCount = 2),
            channelCallBannerOf(activeMucCall(), room, listOf("me", "alice"), videoCall = false),
        )
    }

    @Test
    fun ongoingPillNeverShowsAZeroCount() {
        // The Muji echo for our own active presence may still be in
        // flight the instant the slot turns Active.
        assertEquals(
            ChannelCallBannerState.Ongoing(participantCount = 1),
            channelCallBannerOf(activeMucCall(), room, emptyList(), videoCall = false),
        )
    }

    // ── Retained-call cleanup (web leaveRetainedMucCallAction dock) ──────────

    @Test
    fun retainedGhostStillAdvertisedInMujiOffersTheLeave() {
        assertEquals(
            true,
            shouldOfferRetainedLeave(
                state = CallState.Idle,
                roomJid = room,
                retained = RetainedSessionView(terminatePending = false),
                mujiNicks = listOf("me", "alice"),
                selfNick = "me",
            ),
        )
    }

    @Test
    fun terminatePendingEntryOffersTheLeaveEvenWithoutAMujiGhost() {
        assertEquals(
            true,
            shouldOfferRetainedLeave(
                state = CallState.Idle,
                roomJid = room,
                retained = RetainedSessionView(terminatePending = true),
                mujiNicks = emptyList(),
                selfNick = "me",
            ),
        )
    }

    @Test
    fun retainedLeaveNeverOffersWhileOurLiveCallOwnsTheRoom() {
        assertEquals(
            false,
            shouldOfferRetainedLeave(
                state = activeMucCall(),
                roomJid = room,
                retained = RetainedSessionView(terminatePending = true),
                mujiNicks = listOf("me"),
                selfNick = "me",
            ),
        )
    }

    @Test
    fun aCachedSessionWithoutGhostOrPendingTerminateStaysQuiet() {
        assertEquals(
            false,
            shouldOfferRetainedLeave(
                state = CallState.Idle,
                roomJid = room,
                retained = RetainedSessionView(terminatePending = false),
                mujiNicks = listOf("alice"),
                selfNick = "me",
            ),
        )
        assertEquals(
            false,
            shouldOfferRetainedLeave(
                state = CallState.Idle,
                roomJid = room,
                retained = null,
                mujiNicks = listOf("me"),
                selfNick = "me",
            ),
        )
    }

    @Test
    fun retainedLeaveBannerOutranksJoinButNeverTheOngoingPill() {
        assertEquals(
            ChannelCallBannerState.LeaveRetained(participantCount = 2),
            channelCallBannerOf(
                CallState.Idle, room, listOf("me", "alice"), videoCall = false,
                retainedLeave = true,
            ),
        )
        assertEquals(
            ChannelCallBannerState.LeaveRetained(participantCount = 1),
            channelCallBannerOf(CallState.Idle, room, emptyList(), videoCall = false, retainedLeave = true),
        )
        assertEquals(
            ChannelCallBannerState.Ongoing(participantCount = 1),
            channelCallBannerOf(activeMucCall(), room, listOf("me"), videoCall = false, retainedLeave = true),
        )
    }
}
