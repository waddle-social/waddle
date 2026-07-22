package social.waddle.android.feature.dm

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.IdleAge
import social.waddle.android.client.idleAgeOf
import social.waddle.android.client.presenceShowsIdle
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.conversation.trustedPreviewOriginOf
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.jid.bareJidOf
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddlePresence

/** DM timeline + composer over the shared conversation scaffold. */
@Composable
fun DmScreen(
    peerJid: String,
    name: String,
    onBack: () -> Unit,
    onOpenThread: (threadId: String) -> Unit,
) {
    val graph = LocalAppGraph.current
    val viewModel: DmViewModel = viewModel(
        key = "dm:$peerJid",
        factory = DmViewModel.factory(graph, peerJid),
    )
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val contacts by graph.sessionManager.presenceStore.contacts.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()

    // Capture permissions are requested BEFORE the XEP-0353 propose so
    // the responder's accept can go straight to media; a denial still
    // places the call receive-only (web best-effort capture parity).
    var pendingCallVideo by remember { mutableStateOf(false) }
    val callPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) {
        val video = pendingCallVideo
        scope.launch {
            graph.sessionManager.callStore.startCall(
                peerJid = peerJid,
                media = WaddleCallMedia(audio = true, video = video),
            )
        }
    }

    ConversationScreen(
        title = name,
        viewModel = viewModel,
        onBack = onBack,
        onOpenThread = onOpenThread,
        searchTarget = MessageSearchTarget(peerJid, isGroupchat = false),
        selfBareJid = session?.jid?.let(::bareJidOf),
        subtitle = dmPresenceSubtitle(contacts[peerJid]),
        trustedMediaOrigin = trustedPreviewOriginOf(session, graph.serverUrl),
        onStartCall = { video ->
            pendingCallVideo = video
            callPermissionLauncher.launch(
                if (video) {
                    arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
                } else {
                    arrayOf(Manifest.permission.RECORD_AUDIO)
                },
            )
        },
    )
}

/**
 * Web `ChatHeader.dmPeerPresenceText` parity: "away · idle 20m" while
 * the peer's presence show is `away`/`xa` and XEP-0319 `idleSince` is
 * present; bare "away" without it; nothing otherwise. The idle age
 * re-derives on a minute ticker so it stays current without presence
 * traffic.
 */
@Composable
private fun dmPresenceSubtitle(presence: WaddlePresence?): String? {
    if (presence == null || !presenceShowsIdle(presence.show)) return null
    val idleSince = presence.idleSince
        ?: return stringResource(R.string.dm_presence_away)
    var nowMs by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(idleSince) {
        while (true) {
            delay(MINUTE_MS)
            nowMs = System.currentTimeMillis()
        }
    }
    val age = idleAgeOf(idleSince, nowMs)
        ?: return stringResource(R.string.dm_presence_away)
    return stringResource(R.string.dm_presence_away_idle, idleAgeText(age))
}

@Composable
private fun idleAgeText(age: IdleAge): String = when (age) {
    IdleAge.UnderMinute -> stringResource(R.string.idle_under_minute)
    is IdleAge.Minutes -> stringResource(R.string.idle_minutes, age.minutes)
    is IdleAge.Hours -> stringResource(R.string.idle_hours, age.hours)
    is IdleAge.Days -> stringResource(R.string.idle_days, age.days)
}

private const val MINUTE_MS = 60_000L
