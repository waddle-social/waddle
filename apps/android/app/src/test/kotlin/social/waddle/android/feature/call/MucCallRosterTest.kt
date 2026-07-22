package social.waddle.android.feature.call

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The roster resolution rule (web resolveRoomParticipantList /
 * identitiesToNicks parity): LiveKit identities beat the Muji presence
 * view when populated, owner-mapped nicks beat localpart fallbacks,
 * and the self-leaving marker keeps a local leave monotonic.
 */
class MucCallRosterTest {
    private val room = "room@muc.waddle.test"

    @Test
    fun emptyRoomJidResolvesToNothing() {
        assertTrue(
            resolveRoomParticipantList(
                null,
                mapOf(room to setOf("alice")),
                emptyMap(),
                emptyMap(),
            ).isEmpty(),
        )
        assertTrue(
            resolveRoomParticipantList("", emptyMap(), emptyMap(), emptyMap()).isEmpty(),
        )
    }

    @Test
    fun fallsBackToMujiPresenceWhenNoLiveProjectionExists() {
        val resolved = resolveRoomParticipantList(
            room,
            mapOf(room to linkedSetOf("alice", "bob")),
            emptyMap(),
            emptyMap(),
        )
        assertEquals(listOf("alice", "bob"), resolved)
    }

    @Test
    fun roomJidIsNormalizedBeforeLookup() {
        val resolved = resolveRoomParticipantList(
            "Room@MUC.Waddle.Test/nick",
            mapOf(room to setOf("alice")),
            emptyMap(),
            emptyMap(),
        )
        assertEquals(listOf("alice"), resolved)
    }

    @Test
    fun liveIdentitiesWinOverTheMujiViewAndMapThroughOwners() {
        val resolved = resolveRoomParticipantList(
            room,
            mapOf(room to setOf("stale-muji-nick")),
            mapOf(room to mapOf("alice" to "alice@waddle.test/web", "bob" to "bob@waddle.test/phone")),
            mapOf(room to listOf("alice@waddle.test/web", "bob@waddle.test/phone")),
        )
        assertEquals(listOf("alice", "bob"), resolved)
    }

    @Test
    fun unmappedLiveIdentityDegradesToTheJidLocalpart() {
        val resolved = resolveRoomParticipantList(
            room,
            emptyMap(),
            emptyMap(),
            mapOf(room to listOf("carol@waddle.test/web")),
        )
        // NOT the resource (`web`) — the localpart is the user-facing
        // label until Muji presence resolves the owner mapping.
        assertEquals(listOf("carol"), resolved)
    }

    @Test
    fun ownerMappingIsCaseInsensitiveOnTheBarePartOnly() {
        val resolved = resolveRoomParticipantList(
            room,
            emptyMap(),
            mapOf(room to mapOf("alice" to "Alice@Waddle.Test/Web")),
            mapOf(room to listOf("alice@waddle.test/Web")),
        )
        assertEquals(listOf("alice"), resolved)
    }

    @Test
    fun resourceCaseStaysSignificantPerRfc7622() {
        val resolved = resolveRoomParticipantList(
            room,
            emptyMap(),
            mapOf(room to mapOf("alice" to "alice@waddle.test/WEB")),
            mapOf(room to listOf("alice@waddle.test/web")),
        )
        // Different resources are different identities: no owner match,
        // localpart fallback.
        assertEquals(listOf("alice"), resolved)
    }

    @Test
    fun identicalNicksFromTwoSessionsDeduplicate() {
        val resolved = resolveRoomParticipantList(
            room,
            emptyMap(),
            mapOf(
                room to mapOf("alice" to "alice@waddle.test/web"),
            ),
            mapOf(room to listOf("alice@waddle.test/web", "alice@waddle.test/phone")),
        )
        assertEquals(listOf("alice"), resolved)
    }

    @Test
    fun leavingMarkerSuppressesOurStaleMujiNick() {
        val resolved = resolveRoomParticipantList(
            room,
            mapOf(room to linkedSetOf("me", "bob")),
            emptyMap(),
            emptyMap(),
            mapOf(room to "me"),
        )
        assertEquals(listOf("bob"), resolved)
    }

    @Test
    fun leavingMarkerIsANoOpOnceMujiDroppedTheNick() {
        val resolved = resolveRoomParticipantList(
            room,
            mapOf(room to setOf("bob")),
            emptyMap(),
            emptyMap(),
            mapOf(room to "me"),
        )
        assertEquals(listOf("bob"), resolved)
    }

    @Test
    fun leavingMarkerDoesNotSuppressTheLiveProjection() {
        val resolved = resolveRoomParticipantList(
            room,
            emptyMap(),
            emptyMap(),
            mapOf(room to listOf("me@waddle.test/android")),
            mapOf(room to "me"),
        )
        // A populated live snapshot proves the connection; the marker
        // only guards the Muji fallback.
        assertEquals(listOf("me"), resolved)
    }

    @Test
    fun rosterEntriesCarryRaisedHandAndMuteBadges() {
        val roster = mucRosterOf(
            room,
            MucPresenceRosterView(
                participants = mapOf(room to linkedSetOf("alice", "bob", "carol")),
                owners = emptyMap(),
                raisedHands = mapOf(room to setOf("bob")),
                mutedNicks = mapOf(room to setOf("carol")),
            ),
            LiveRosterView(participants = emptyMap(), leavingRooms = emptyMap()),
        )
        assertEquals(
            listOf(
                MucRosterEntry("alice", handRaised = false, muted = false),
                MucRosterEntry("bob", handRaised = true, muted = false),
                MucRosterEntry("carol", handRaised = false, muted = true),
            ),
            roster,
        )
    }

    @Test
    fun storeSnapshotsDedupeAndNormalizeIdentities() {
        val store = MucCallLiveParticipantsStore()
        store.setParticipants(room, listOf("Alice@Waddle.Test/web", "alice@waddle.test/web", ""))
        assertEquals(
            mapOf(room to listOf("alice@waddle.test/web")),
            store.participants.value,
        )
    }

    @Test
    fun emptySnapshotDropsTheRoomEntry() {
        val store = MucCallLiveParticipantsStore()
        store.setParticipants(room, listOf("alice@waddle.test/web"))
        store.setParticipants(room, emptyList())
        assertTrue(store.participants.value.isEmpty())
    }

    @Test
    fun nonEmptySnapshotConsumesAStaleLeaveMarker() {
        val store = MucCallLiveParticipantsStore()
        store.markLeaving(room, "me")
        assertEquals(mapOf(room to "me"), store.leavingRooms.value)
        store.setParticipants(room, listOf("me@waddle.test/android"))
        assertTrue(store.leavingRooms.value.isEmpty())
    }

    @Test
    fun markLeavingWithoutANickIsANoOp() {
        val store = MucCallLiveParticipantsStore()
        store.markLeaving(room, null)
        store.markLeaving(room, "")
        assertTrue(store.leavingRooms.value.isEmpty())
    }
}
