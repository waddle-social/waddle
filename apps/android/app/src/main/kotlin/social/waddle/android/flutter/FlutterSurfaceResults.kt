package social.waddle.android.flutter

import social.waddle.android.client.CreateRoomResult
import social.waddle.android.client.RoomAdminResult
import social.waddle.android.client.VerbResult

internal fun checked(result: VerbResult): Any? {
    check(result == VerbResult.Ok) {
        if (result == VerbResult.NotConnected) {
            "Disconnected. Try again when connected"
        } else {
            "Server refused the action"
        }
    }
    return null
}

internal fun checked(result: RoomAdminResult): Any? {
    check(result == RoomAdminResult.Ok) {
        when (result) {
            RoomAdminResult.NotConnected -> "Disconnected. Try again when connected"
            RoomAdminResult.NotPermitted -> "You do not have permission for this action"
            else -> "Server refused the action"
        }
    }
    return null
}

internal fun created(result: CreateRoomResult): Map<String, String> =
    when (result) {
        is CreateRoomResult.Created -> mapOf("jid" to result.roomJid)
        CreateRoomResult.InvalidName -> error("Choose a valid room name")
        CreateRoomResult.NotConnected -> error("Disconnected. Try again when connected")
        CreateRoomResult.NotPermitted -> error("You do not have permission to create this room")
        CreateRoomResult.Rejected -> error("Server refused room creation")
    }
