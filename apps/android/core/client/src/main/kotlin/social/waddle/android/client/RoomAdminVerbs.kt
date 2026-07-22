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

    /**
     * `urn:waddle:group-dm:create:0`: create a hidden members-only
     * group DM. [memberJids] is the full membership including the
     * caller (server dedups; at least two distinct JIDs). On success
     * the topology is re-discovered so the room lands in the store via
     * its server-written bookmark before the caller joins it.
     */
    suspend fun createGroupDm(name: String, memberJids: List<String>): CreateRoomResult {
        val client = activeSession.client ?: return CreateRoomResult.NotConnected
        val trimmed = name.trim()
        if (trimmed.isEmpty()) return CreateRoomResult.InvalidName
        val roomJid = try {
            client.createGroupDm(trimmed, memberJids.map(::bareJid))
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (error: WaddleException) {
            return when (error) {
                is WaddleException.NotConnected -> CreateRoomResult.NotConnected
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

    /**
     * `urn:waddle:group-dm:rename:0` (IQ to the room; `null`/blank
     * clears the custom name). The server rewrites every member's
     * bookmark, so the topology refresh picks the new name up for the
     * DM surface.
     */
    suspend fun renameGroupDm(roomJid: String, name: String?): RoomAdminResult {
        val result = adminCall { client ->
            client.renameGroupDm(bareJid(roomJid), name?.trim()?.takeIf { it.isNotEmpty() })
        }
        if (result == RoomAdminResult.Ok) refreshTopology()
        return result
    }

    /**
     * `urn:waddle:group-dm:leave:0` (IQ to the server domain). On Ok
     * the server has retracted our bookmark: mark the room left and
     * refresh the topology so it drops off the DM surface.
     */
    suspend fun leaveGroupDm(roomJid: String): RoomAdminResult {
        val result = adminCall { client -> client.leaveGroupDm(bareJid(roomJid)) }
        if (result == RoomAdminResult.Ok) {
            stores.roomStore.markLeft(roomJid)
            refreshTopology()
        }
        return result
    }

    /**
     * XEP-0045 §7.8.2 mediated invite adding [inviteeJid] to a group
     * DM; [fullHistory] requests the full-archive grant via the Waddle
     * history-access extension (the server downgrades ineligible
     * requests to from-join).
     */
    suspend fun inviteToGroupDm(
        roomJid: String,
        inviteeJid: String,
        fullHistory: Boolean,
    ): RoomAdminResult = adminCall { client ->
        client.inviteToGroupDm(bareJid(roomJid), bareJid(inviteeJid), fullHistory)
    }

    /** Re-discover the space/channel topology into the room store. */
    suspend fun refreshTopology() {
        // Generation-gated like every other post-wire store write
        // (ProfileVerbs precedent): a discovery answering after a
        // logout/relogin must not park the previous account's rooms
        // in the freshly seeded store.
        val generation = activeSession.generation
        val topology = activeSession.fetch { client -> client.discoverTopology() } ?: return
        if (activeSession.generation != generation) return
        stores.roomStore.setTopology(topology)
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
