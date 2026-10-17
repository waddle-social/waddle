package social.waddle.android.feature.conversation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.jid.bareJidOf

/** One thread over the shared conversation scaffold (no nesting); [onOpenDm] backs "Message" on profiles. */
@Composable
fun ThreadScreen(
    conversationJid: String,
    isGroupchat: Boolean,
    threadId: String,
    onBack: () -> Unit,
    onOpenDm: ((peerJid: String, name: String) -> Unit)? = null,
) {
    val graph = LocalAppGraph.current
    val viewModel: ThreadViewModel = viewModel(
        key = "thread:$conversationJid:$threadId",
        factory = ThreadViewModel.factory(graph, conversationJid, isGroupchat, threadId),
    )
    val session by graph.currentSession.collectAsStateWithLifecycle()
    ConversationScreen(
        title = stringResource(R.string.thread_title),
        viewModel = viewModel,
        onBack = onBack,
        onOpenDm = onOpenDm,
        selfBareJid = session?.jid?.let(::bareJidOf),
    )
}
