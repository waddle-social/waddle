package social.waddle.android.client

import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleMucRole
import social.waddle.client.ffi.WaddlePresence

/** XEP-0045 §7.2.2 status code marking the recipient's own presence. */
private const val SELF_PRESENCE_STATUS: UShort = 110u

/**
 * The account's own occupant presence in a room, identified by MUC
 * status code 110 on the `muc#user` payload. `null` while not joined
 * (or before the self-presence echo arrives) — callers MUST treat
 * that as "no privileges" for gating purposes.
 */
fun selfOccupantOf(occupants: Map<String, WaddlePresence>): WaddlePresence? =
    occupants.values.firstOrNull { SELF_PRESENCE_STATUS in it.mucStatusCodes }

/**
 * Web `canManageMembers` parity: own affiliation ∈ {owner, admin}.
 * UI gating only — the server enforces the §5.2 privilege matrix on
 * every muc#admin set regardless.
 */
fun canManageMembersOf(occupants: Map<String, WaddlePresence>): Boolean =
    selfOccupantOf(occupants)?.mucAffiliation.let {
        it == WaddleMucAffiliation.OWNER || it == WaddleMucAffiliation.ADMIN
    }

/** Room-settings gating: own affiliation is owner (§10 owner use cases). */
fun isRoomOwnerOf(occupants: Map<String, WaddlePresence>): Boolean =
    selfOccupantOf(occupants)?.mucAffiliation == WaddleMucAffiliation.OWNER

/**
 * XEP-0425 moderation gating: moderators (role) or owners/admins
 * (affiliation) may retract others' messages.
 */
fun canModerateOf(occupants: Map<String, WaddlePresence>): Boolean {
    val self = selfOccupantOf(occupants) ?: return false
    return self.mucRole == WaddleMucRole.MODERATOR ||
        self.mucAffiliation == WaddleMucAffiliation.OWNER ||
        self.mucAffiliation == WaddleMucAffiliation.ADMIN
}
