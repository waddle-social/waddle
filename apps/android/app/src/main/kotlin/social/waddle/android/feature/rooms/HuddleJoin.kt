package social.waddle.android.feature.rooms

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewmodel.compose.viewModel
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
    // Held in a ViewModel: it survives the recreation a rotation causes
    // while the permission dialog is up, but not process death, after
    // which the tap is stale. Deliberately not in saved state.
    val pending: PendingHuddleJoin = viewModel()
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
        val room = pending.room ?: return@rememberLauncherForActivityResult
        pending.room = null
        joinHuddle(graph, room.roomJid, video = room.video)
        opened(room)
    }
    return { room ->
        pending.room = room
        launcher.launch(
            if (room.video) {
                arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
            } else {
                arrayOf(Manifest.permission.RECORD_AUDIO)
            },
        )
    }
}

/** The room a Join tap is waiting on a permission result for. */
internal class PendingHuddleJoin : ViewModel() {
    var room: RoomActivity? = null
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
