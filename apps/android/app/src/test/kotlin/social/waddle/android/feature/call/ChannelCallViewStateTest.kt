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

    private val selfFullJid = "me@waddle.test/android"
    private val deadResourceJid = "me@waddle.test/waddle-android-old"

    private fun offersRetainedLeave(
        state: CallState = CallState.Idle,
        retained: RetainedSessionView? = RetainedSessionView(
            terminatePending = false,
            selfFullJid = deadResourceJid,
        ),
        mujiNicks: List<String> = listOf("me", "alice"),
        selfNick: String? = "me",
        owners: Map<String, String?> = emptyMap(),
    ): Boolean = shouldOfferRetainedLeave(
        state = state,
        roomJid = room,
        retained = retained,
        muji = RoomMujiView(nicks = mujiNicks, owners = owners),
        self = SelfCallIdentity(nick = selfNick, fullJid = selfFullJid),
    )

    @Test
    fun deadResourceOwnedGhostOffersTheLeave() {
        assertEquals(
            true,
            offersRetainedLeave(
                owners = mapOf("me" to deadResourceJid, "alice" to "alice@waddle.test/pc"),
            ),
        )
    }

    @Test
    fun hiddenOwnerFallsBackToTheGhostReading() {
        // The room hides real JIDs: the cache + presence evidence wins.
        assertEquals(true, offersRetainedLeave(owners = emptyMap()))
    }

    @Test
    fun anotherLiveResourceSharingTheNickNeverOffersTheLeave() {
        // Our bare JID, but a resource that is neither the cached dead
        // one nor the current bound one: a sibling device is live in
        // the call — the banner must offer Join, not a destructive leave.
        assertEquals(
            false,
            offersRetainedLeave(owners = mapOf("me" to "me@waddle.test/tablet")),
        )
    }

    @Test
    fun aForeignOccupantSharingTheNickNeverOffersTheLeave() {
        assertEquals(
            false,
            offersRetainedLeave(owners = mapOf("me" to "eve@waddle.test/pc")),
        )
    }

    @Test
    fun presenceOnlyGhostWithoutACacheEntryStillOffersTheLeave() {
        // Process death BEFORE the mixer accept caches nothing, but the
        // active presence already hit the room; the stable Android
        // resource means the ghost's owner IS our current full JID.
        assertEquals(
            true,
            offersRetainedLeave(retained = null, owners = mapOf("me" to selfFullJid)),
        )
    }

    @Test
    fun terminatePendingEntryOffersTheLeaveEvenWithoutAMujiGhost() {
        assertEquals(
            true,
            offersRetainedLeave(
                retained = RetainedSessionView(terminatePending = true, selfFullJid = deadResourceJid),
                mujiNicks = emptyList(),
            ),
        )
    }

    @Test
    fun retainedLeaveNeverOffersWhileOurLiveCallOwnsTheRoom() {
        assertEquals(
            false,
            offersRetainedLeave(
                state = activeMucCall(),
                retained = RetainedSessionView(terminatePending = true, selfFullJid = deadResourceJid),
                mujiNicks = listOf("me"),
                owners = mapOf("me" to deadResourceJid),
            ),
        )
    }

    @Test
    fun aCachedSessionWithoutGhostOrPendingTerminateStaysQuiet() {
        // Our nick is not advertised at all.
        assertEquals(false, offersRetainedLeave(mujiNicks = listOf("alice")))
        // No cache entry and the advertised nick belongs to a live
        // sibling resource: nothing retained to clean up.
        assertEquals(
            false,
            offersRetainedLeave(retained = null, owners = mapOf("me" to "me@waddle.test/tablet")),
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
