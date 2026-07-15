package social.waddle.android.feature.conversation

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R

/** One thread over the shared conversation scaffold (no nesting). */
@Composable
fun ThreadScreen(
    conversationJid: String,
    isGroupchat: Boolean,
    threadId: String,
    onBack: () -> Unit,
) {
    val graph = LocalAppGraph.current
    val viewModel: ThreadViewModel = viewModel(
        key = "thread:$conversationJid:$threadId",
        factory = ThreadViewModel.factory(graph, conversationJid, isGroupchat, threadId),
    )
    ConversationScreen(
        title = stringResource(R.string.thread_title),
        viewModel = viewModel,
        onBack = onBack,
    )
}
