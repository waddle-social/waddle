package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.store.SessionStores
import social.waddle.client.ffi.WaddleAdminUsersPage
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleException
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleRoomConfig
import social.waddle.client.ffi.WaddleRoomConfigPatch
import social.waddle.client.ffi.WaddleRoomMemberEntry
import social.waddle.client.ffi.WaddleUserSearchEntry

/** Web `createChannel` slug parity: lowercase, whitespace → hyphens. */
fun channelLocalpartOf(name: String): String =
    name.trim().lowercase().replace(Regex("\\s+"), "-")

/**
 * Room lifecycle + member management passthroughs (XEP-0045 §8/§9/§10,
 * XEP-0055 user search, and the `urn:waddle:admin:*` owner surface).
 * Same shape rules as [ConversationVerbs]: never throws past the
 * seam — every call collapses to a typed result or `null`.
 */
internal class RoomAdminVerbs(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
) {
    /**
     * Refresh a room's member list with the web `listRoomMembers`
     * fan-out: one `muc#admin` query per affiliation tier, tolerating
     * per-tier failures (a room commonly forbids e.g. the outcast
     * query to non-admins). The store degrades to `UNAVAILABLE` only
     * when every tier failed and nothing was collected.
     */
    suspend fun refreshRoomMembers(roomJid: String) {
        val client = activeSession.client
        if (client == null) {
            stores.roomMembersStore.applyUnavailable(roomJid)
            return
        }
        stores.roomMembersStore.markLoading(roomJid)
        val members = mutableListOf<WaddleRoomMemberEntry>()
        var failures = 0
        for (tier in MEMBER_LIST_TIERS) {
            try {
                members += client.listRoomMembers(roomJid, tier)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                failures += 1
            }
        }
        if (members.isEmpty() && failures > 0) {
            stores.roomMembersStore.applyUnavailable(roomJid)
        } else {
            stores.roomMembersStore.applyLoaded(roomJid, members)
        }
    }

    /** XEP-0045 §5.2 affiliation change (ban = §9.1 `OUTCAST`, remove = `NONE`). */
    suspend fun setRoomAffiliation(
        roomJid: String,
        targetJid: String,
        affiliation: WaddleMucAffiliation,
        reason: String?,
    ): RoomAdminResult = adminCall { client ->
        client.setRoomAffiliation(bareJid(roomJid), bareJid(targetJid), affiliation, reason)
    }

    /** XEP-0045 §8.2 kick: eject by nick, role → none, affiliation kept. */
    suspend fun kickOccupant(roomJid: String, nick: String, reason: String?): RoomAdminResult =
        adminCall { client -> client.kickOccupant(bareJid(roomJid), nick, reason) }

    /** XEP-0045 §10.2 owner config fetch; `null` offline / not owner. */
    suspend fun fetchRoomConfig(roomJid: String): WaddleRoomConfig? =
        activeSession.fetch { client -> client.fetchRoomConfig(bareJid(roomJid)) }

    /** XEP-0045 §10.2 GET-merge-SET submit of an owner edit patch. */
    suspend fun submitRoomConfig(roomJid: String, patch: WaddleRoomConfigPatch): RoomAdminResult =
        adminCall { client -> client.submitRoomConfig(bareJid(roomJid), patch) }

    /**
     * XEP-0045 §10.1 room creation (web `createMucRoom` parity): the
     * localpart is the naive name slug, the initial config carries
     * name/description/forum. On success the topology is re-discovered
     * so the new channel appears in the Home list.
     */
    suspend fun createRoom(
        name: String,
        nick: String,
        description: String?,
        forum: Boolean,
    ): CreateRoomResult {
        val client = activeSession.client ?: return CreateRoomResult.NotConnected
        val localpart = channelLocalpartOf(name)
        if (localpart.isEmpty()) return CreateRoomResult.InvalidName
        val patch = WaddleRoomConfigPatch(
            name = name.trim(),
            description = description?.trim()?.takeIf { it.isNotEmpty() },
            forum = forum,
            pinPermission = null,
        )
        val roomJid = try {
            client.createRoom(localpart, nick, patch)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (error: WaddleException) {
            return when (error) {
                is WaddleException.NotConnected -> CreateRoomResult.NotConnected
                is WaddleException.InvalidJid -> CreateRoomResult.InvalidName
                is WaddleException.Stanza ->
                    if (error.condition in PERMISSION_CONDITIONS) {
                        CreateRoomResult.NotPermitted
                    } else {
                        CreateRoomResult.Rejected
                    }
                else -> CreateRoomResult.Rejected
            }
        } catch (_: Throwable) {
            return CreateRoomResult.Rejected
        }
        refreshTopology()
        return CreateRoomResult.Created(roomJid)
    }

    /** XEP-0045 §10.9 destroy; refreshes the topology on success. */
    suspend fun destroyRoom(roomJid: String, reason: String?): RoomAdminResult {
        val result = adminCall { client -> client.destroyRoom(bareJid(roomJid), reason) }
        if (result == RoomAdminResult.Ok) {
            stores.roomStore.markLeft(roomJid)
            refreshTopology()
        }
        return result
    }

    /** Re-discover the space/channel topology into the room store. */
    suspend fun refreshTopology() {
        activeSession.fetch { client -> client.discoverTopology() }
            ?.let(stores.roomStore::setTopology)
    }

    /** XEP-0055 user search (`nick` column); `null` offline/failed. */
    suspend fun searchUsers(query: String): List<WaddleUserSearchEntry>? =
        activeSession.fetch { client -> client.searchUsers(query) }

    /**
     * Best-effort community-owner probe (`urn:waddle:admin:users:list:0`
     * with `page_size=1`). Gates the admin UI entry points only — the
     * server re-authorizes every actual command, so a false positive
     * cannot escalate anything.
     */
    suspend fun isCommunityOwner(): Boolean =
        activeSession.fetch { client -> client.isCommunityOwner() } ?: false

    /** V1 admin users page; `null` offline / not owner / failed. */
    suspend fun adminUsersList(
        prefix: String?,
        pageSize: UInt?,
        afterCursor: String?,
    ): WaddleAdminUsersPage? =
        activeSession.fetch { client -> client.adminUsersList(prefix, pageSize, afterCursor) }

    /** Collapse a throwing FFI admin call into [RoomAdminResult]. */
    private suspend fun adminCall(
        op: suspend (WaddleClientInterface) -> Unit,
    ): RoomAdminResult {
        val client = activeSession.client ?: return RoomAdminResult.NotConnected
        return try {
            op(client)
            RoomAdminResult.Ok
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (error: WaddleException) {
            when (error) {
                is WaddleException.NotConnected -> RoomAdminResult.NotConnected
                is WaddleException.Stanza ->
                    if (error.condition in PERMISSION_CONDITIONS) {
                        RoomAdminResult.NotPermitted
                    } else {
                        RoomAdminResult.Rejected
                    }
                else -> RoomAdminResult.Rejected
            }
        } catch (_: Throwable) {
            RoomAdminResult.Rejected
        }
    }

    private companion object {
        /** §9.5 fan-out order; owners render first (web sort parity). */
        val MEMBER_LIST_TIERS = listOf(
            WaddleMucAffiliation.OWNER,
            WaddleMucAffiliation.ADMIN,
            WaddleMucAffiliation.MEMBER,
            WaddleMucAffiliation.OUTCAST,
        )

        /** RFC 6120 conditions meaning "insufficient privileges". */
        val PERMISSION_CONDITIONS = setOf("forbidden", "not-allowed")
    }
}
