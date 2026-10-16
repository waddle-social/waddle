package social.waddle.android.feature.rooms

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.DEFAULT_NICK
import social.waddle.android.LocalAppGraph
import social.waddle.android.jid.domainOf
import social.waddle.client.ffi.WaddleCallMedia

/**
 * "Join" on a live room, from outside the room: ask for the microphone
 * (and camera for a video huddle) first, then run the XEP-0272 join on
 * the application scope, then [onJoined] (open the room). A denial
 * still joins, receive-only, as the in-room join does. Navigation waits
 * for the permission result: leaving first would drop the launcher.
 */
@Composable
fun rememberHuddleJoiner(onJoined: (room: RoomActivity) -> Unit): (RoomActivity) -> Unit {
    val graph = LocalAppGraph.current
    val opened by rememberUpdatedState(onJoined)
    // Not saveable on purpose: after process death the tap is stale.
    var pending by remember { mutableStateOf<RoomActivity?>(null) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
        val room = pending ?: return@rememberLauncherForActivityResult
        pending = null
        joinHuddle(graph, room.roomJid, video = room.video)
        opened(room)
    }
    return { room ->
        pending = room
        launcher.launch(
            if (room.video) {
                arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
            } else {
                arrayOf(Manifest.permission.RECORD_AUDIO)
            },
        )
    }
}

/**
 * The in-room join's flow (ChannelScreen `startOrJoinMucCall`), run on
 * the application scope because the setup outlives this screen:
 * resume a cached session first, else the full §Joining flow.
 */
private fun joinHuddle(graph: AppGraph, roomJid: String, video: Boolean) {
    val nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK
    val accountJid = graph.currentSession.value?.jid
    val selfFullJid = graph.sessionManager.ownFullJid()
    val store = graph.sessionManager.callStore
    graph.applicationScope.launch {
        if (store.muc.resume(roomJid, nick, selfFullJid)) return@launch
        store.muc.begin(
            roomJid = roomJid,
            media = WaddleCallMedia(audio = true, video = video),
            selfNick = nick,
            selfFullJid = selfFullJid,
            expectedMixerJid = accountJid
                ?.let(::domainOf)
                ?.takeIf { it.isNotEmpty() }
                ?.let { domain -> "calls.$domain" },
        )
    }
}
