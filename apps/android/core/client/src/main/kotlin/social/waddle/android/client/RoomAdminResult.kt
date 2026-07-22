package social.waddle.android.client

/**
 * Outcome of a room admin action (XEP-0045 affiliation change, kick,
 * owner config submit, destroy). Distinguishes "the server refused
 * because the caller lacks privileges" from generic failure so screens
 * can explain a `forbidden`/`not-allowed` instead of a vague error.
 */
sealed interface RoomAdminResult {
    data object Ok : RoomAdminResult

    data object NotConnected : RoomAdminResult

    /** RFC 6120 `forbidden` / `not-allowed`: insufficient privileges. */
    data object NotPermitted : RoomAdminResult

    data object Rejected : RoomAdminResult
}

/** Outcome of a XEP-0045 §10.1 room creation. */
sealed interface CreateRoomResult {
    /** The room exists and was configured; [roomJid] is its bare JID. */
    data class Created(val roomJid: String) : CreateRoomResult

    data object NotConnected : CreateRoomResult

    /** The derived localpart is not a valid JID node. */
    data object InvalidName : CreateRoomResult

    /** The name collides with a room the caller does not own, or the
     *  caller may not create rooms (`forbidden` / `not-allowed`). */
    data object NotPermitted : CreateRoomResult

    data object Rejected : CreateRoomResult
}
