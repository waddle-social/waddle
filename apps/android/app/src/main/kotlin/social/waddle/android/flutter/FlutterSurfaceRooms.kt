package social.waddle.android.flutter

import social.waddle.android.AppGraph
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddlePinPermission
import social.waddle.client.ffi.WaddleRoomConfigPatch

/** Directory and room management reuse the existing XMPP administration APIs. */
internal class FlutterSurfaceRooms(private val graph: AppGraph) {
    private val manager = graph.sessionManager

    suspend fun execute(method: String, args: Map<String, Any?>): Any? =
        when (method.substringBefore('.')) {
            "directory" -> directoryCommand(method, args)
            "admin" -> adminCommand(method, args)
            "room" -> roomCommand(method, args)
            "group" -> groupCommand(method, args)
            else -> error("Unknown room command")
        }

    private fun ownNick(): String =
        graph.currentSession.value?.xmppLocalpart ?: error("Sign in first")

    private suspend fun directoryCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "directory.search" ->
                (
                    manager.searchUsers(args.text("query"))
                        ?: error("Directory unavailable while disconnected")
                )
                    .map {
                        mapOf(
                            "jid" to it.jid,
                            "name" to (it.displayName ?: it.username),
                            "username" to it.username,
                        )
                    }
            else -> error("Unknown directory command")
        }

    private suspend fun adminCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "admin.users" -> {
                val page =
                    manager.adminUsersList(
                        args.optional("prefix"),
                        50u,
                        args.optional("afterCursor"),
                    ) ?: error("User directory unavailable or permission denied")
                mapOf(
                    "users" to
                        page.entries.map {
                            mapOf(
                                "jid" to it.jid,
                                "username" to it.jid.substringBefore('@'),
                                "name" to it.displayName,
                                "role" to if (it.hasOwnerHat) "Owner" else "Member",
                            )
                        },
                    "nextCursor" to page.nextCursor,
                )
            }
            else -> error("Unknown admin command")
        }

    private suspend fun roomCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "room.create" ->
                created(
                    manager.createRoom(
                        args.required("name"),
                        ownNick(),
                        args.optional("description"),
                        args["forum"] == true,
                    )
                )
            "room.config" ->
                manager.fetchRoomConfig(args.required("jid"))?.let {
                    mapOf(
                        "name" to it.name,
                        "description" to it.description,
                        "forum" to it.forum,
                        "membersOnly" to it.membersOnly,
                        "publicRoom" to it.publicRoom,
                        "moderated" to it.moderated,
                        "pinPermission" to it.pinPermission?.name?.lowercase(),
                    )
                } ?: error("Room settings unavailable or permission denied")
            "room.save" ->
                checked(
                    manager.submitRoomConfig(
                        args.required("jid"),
                        WaddleRoomConfigPatch(
                            name = args["name"] as? String,
                            description = args["description"] as? String,
                            forum = args["forum"] as? Boolean,
                            pinPermission =
                                args.optional("pinPermission")?.let {
                                    WaddlePinPermission.valueOf(it.uppercase())
                                },
                        ),
                    )
                )
            "room.destroy" ->
                checked(manager.destroyRoom(args.required("jid"), args.optional("reason")))
            "room.affiliation" ->
                checked(
                    manager.setRoomAffiliation(
                        args.required("jid"),
                        args.required("targetJid"),
                        WaddleMucAffiliation.valueOf(args.required("affiliation").uppercase()),
                        args.optional("reason"),
                    )
                )
            "room.kick" ->
                checked(
                    manager.kickOccupant(
                        args.required("jid"),
                        args.required("nick"),
                        args.optional("reason"),
                    )
                )
            else -> error("Unknown room command")
        }

    private suspend fun groupCommand(method: String, args: Map<String, Any?>): Any? =
        when (method) {
            "group.create" ->
                created(manager.createGroupDm(args.text("name"), args.strings("members")))
            "group.rename" ->
                checked(manager.renameGroupDm(args.required("jid"), args.optional("name")))
            "group.invite" ->
                checked(
                    manager.inviteToGroupDm(
                        args.required("jid"),
                        args.required("inviteeJid"),
                        args["fullHistory"] == true,
                    )
                )
            "group.leave" -> checked(manager.leaveGroupDm(args.required("jid")))
            else -> error("Unknown group command")
        }
}
