package social.waddle.android.feature.conversation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.res.pluralStringResource
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
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    // The feed is oldest first: the first stored row is the root.
    val root = state.entries.firstNotNullOfOrNull { (it.row as? ConversationRow.Stored)?.item }
    ConversationScreen(
        title = stringResource(R.string.thread_title),
        titleSubline = root?.let { item ->
            {
                val replies = (state.entries.size - 1).coerceAtLeast(0)
                DmHeaderSubline(
                    pluralStringResource(R.plurals.thread_replies_count, replies, replies, authorNameOf(item)),
                )
            }
        },
        viewModel = viewModel,
        onBack = onBack,
        onOpenDm = onOpenDm,
        selfBareJid = session?.jid?.let(::bareJidOf),
    )
}
