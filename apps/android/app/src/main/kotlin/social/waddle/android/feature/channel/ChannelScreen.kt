package social.waddle.android.feature.channel

import androidx.compose.runtime.Composable
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.search.MessageSearchTarget

/** Channel timeline + composer over the shared conversation scaffold. */
@Composable
fun ChannelScreen(
    roomJid: String,
    name: String,
    onBack: () -> Unit,
    onOpenThread: (threadId: String) -> Unit,
) {
    val graph = LocalAppGraph.current
    val viewModel: ChannelViewModel = viewModel(
        key = "channel:$roomJid",
        factory = ChannelViewModel.factory(graph, roomJid),
    )
    ConversationScreen(
        title = name,
        viewModel = viewModel,
        onBack = onBack,
        onOpenThread = onOpenThread,
        searchTarget = MessageSearchTarget(roomJid, isGroupchat = true),
    )
}
