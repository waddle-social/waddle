package social.waddle.android.feature.dm

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.launch
import social.waddle.android.LocalAppGraph
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.jid.bareJidOf
import social.waddle.client.ffi.WaddleCallMedia

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
