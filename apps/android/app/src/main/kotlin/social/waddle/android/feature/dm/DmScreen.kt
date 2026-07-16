package social.waddle.android.feature.dm

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.jid.bareJidOf

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
    ConversationScreen(
        title = name,
        viewModel = viewModel,
        onBack = onBack,
        onOpenThread = onOpenThread,
        searchTarget = MessageSearchTarget(peerJid, isGroupchat = false),
        selfBareJid = session?.jid?.let(::bareJidOf),
    )
}
