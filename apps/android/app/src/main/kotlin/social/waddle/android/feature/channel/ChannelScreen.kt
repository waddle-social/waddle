package social.waddle.android.feature.channel

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.room.RoomSettingsSheet
import social.waddle.android.feature.room.RoomSettingsViewModel
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.jid.bareJidOf

/** Semantics tags shared with instrumented tests. */
object ChannelScreenTestTags {
    const val MEMBERS_ACTION = "channel-members-action"
    const val ROOM_SETTINGS_ACTION = "channel-room-settings-action"
}

/** Channel timeline + composer over the shared conversation scaffold. */
@Composable
fun ChannelScreen(
    roomJid: String,
    name: String,
    onBack: () -> Unit,
    onOpenThread: (threadId: String) -> Unit,
    onOpenMembers: () -> Unit = {},
) {
    val graph = LocalAppGraph.current
    val viewModel: ChannelViewModel = viewModel(
        key = "channel:$roomJid",
        factory = ChannelViewModel.factory(graph, roomJid),
    )
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val isRoomOwner by viewModel.isRoomOwner.collectAsStateWithLifecycle()
    var roomSettingsOpen by remember { mutableStateOf(false) }

    ConversationScreen(
        title = name,
        viewModel = viewModel,
        onBack = onBack,
        onOpenThread = onOpenThread,
        searchTarget = MessageSearchTarget(roomJid, isGroupchat = true),
        selfBareJid = session?.jid?.let(::bareJidOf),
        extraTopBarActions = {
            IconButton(
                onClick = onOpenMembers,
                modifier = Modifier.testTag(ChannelScreenTestTags.MEMBERS_ACTION),
            ) {
                Icon(
                    Icons.Outlined.Group,
                    contentDescription = stringResource(R.string.members_action),
                )
            }
            // XEP-0045 §10 owner use cases: settings entry is
            // owner-gated by the self-presence affiliation; the
            // server enforces ownership on every config IQ anyway.
            if (isRoomOwner) {
                IconButton(
                    onClick = { roomSettingsOpen = true },
                    modifier = Modifier.testTag(ChannelScreenTestTags.ROOM_SETTINGS_ACTION),
                ) {
                    Icon(
                        Icons.Outlined.Settings,
                        contentDescription = stringResource(R.string.room_settings_title),
                    )
                }
            }
        },
    )

    if (roomSettingsOpen) {
        val settingsViewModel: RoomSettingsViewModel = viewModel(
            key = "room-settings:$roomJid",
            factory = RoomSettingsViewModel.factory(graph, roomJid),
        )
        RoomSettingsSheet(
            viewModel = settingsViewModel,
            onDismiss = { roomSettingsOpen = false },
            onDestroyed = {
                roomSettingsOpen = false
                onBack()
            },
        )
    }
}
