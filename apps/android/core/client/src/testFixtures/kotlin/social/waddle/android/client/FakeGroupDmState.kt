package social.waddle.android.client

import java.util.concurrent.CopyOnWriteArrayList

/**
 * Group-DM lifecycle fake state for [FakeWaddleClient]: recorders and
 * failure knobs for the `urn:waddle:group-dm:*` verbs plus the
 * mediated add-member invite. Extracted so the fake of the full
 * generated FFI interface stays within the LargeClass budget (the
 * [FakeInboxState] / [FakeTopologyState] pattern).
 */
class FakeGroupDmState {
    /** Recorded (name, memberJids) creates. */
    val createCalls = CopyOnWriteArrayList<Pair<String, List<String>>>()

    @Volatile
    var createFailure: Throwable? = null

    /** Bare room JID returned by a successful create. */
    @Volatile
    var createdRoomJid: String = "gdm-1@muc.waddle.test"

    /** Recorded (roomJid, name) renames (`null` = clear). */
    val renameCalls = CopyOnWriteArrayList<Pair<String, String?>>()

    @Volatile
    var renameFailure: Throwable? = null

    /** Recorded room JIDs of leave calls. */
    val leaveCalls = CopyOnWriteArrayList<String>()

    @Volatile
    var leaveFailure: Throwable? = null

    /** Recorded (roomJid, inviteeJid, fullHistory) invites. */
    val inviteCalls = CopyOnWriteArrayList<Triple<String, String, Boolean>>()

    @Volatile
    var inviteFailure: Throwable? = null

    fun createGroupDm(name: String, memberJids: List<String>): String {
        createCalls += name to memberJids
        createFailure?.let { throw it }
        return createdRoomJid
    }

    fun renameGroupDm(roomJid: String, name: String?) {
        renameCalls += roomJid to name
        renameFailure?.let { throw it }
    }

    fun leaveGroupDm(roomJid: String) {
        leaveCalls += roomJid
        leaveFailure?.let { throw it }
    }

    fun inviteToGroupDm(roomJid: String, inviteeJid: String, fullHistory: Boolean) {
        inviteCalls += Triple(roomJid, inviteeJid, fullHistory)
        inviteFailure?.let { throw it }
    }
}
