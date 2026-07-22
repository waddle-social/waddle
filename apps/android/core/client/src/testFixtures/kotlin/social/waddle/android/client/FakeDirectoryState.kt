package social.waddle.android.client

import social.waddle.client.ffi.WaddleAdminUsersPage
import social.waddle.client.ffi.WaddleUserSearchEntry
import java.util.concurrent.CopyOnWriteArrayList

/**
 * User-directory fake state for [FakeWaddleClient]: the XEP-0055
 * search plus the community-owner probe and V1 admin users list.
 * Extracted so the fake of the full generated FFI interface stays
 * within the LargeClass budget (the [FakeInboxState] /
 * [FakeTopologyState] pattern).
 */
class FakeDirectoryState {
    /** Canned XEP-0055 hits; recorded queries. */
    @Volatile
    var userSearchResults: List<WaddleUserSearchEntry> = emptyList()
    val searchUsersCalls = CopyOnWriteArrayList<String>()

    /** Canned owner-probe answer + V1 users page. */
    @Volatile
    var communityOwner = false

    @Volatile
    var adminUsersPage: WaddleAdminUsersPage = WaddleAdminUsersPage(entries = emptyList(), nextCursor = null)

    /** Recorded (prefix, pageSize, afterCursor) users-list queries. */
    val adminUsersListCalls = CopyOnWriteArrayList<Triple<String?, UInt?, String?>>()

    fun searchUsers(query: String): List<WaddleUserSearchEntry> {
        searchUsersCalls += query
        return userSearchResults
    }

    fun adminUsersList(prefix: String?, pageSize: UInt?, afterCursor: String?): WaddleAdminUsersPage {
        adminUsersListCalls += Triple(prefix, pageSize, afterCursor)
        return adminUsersPage
    }
}
