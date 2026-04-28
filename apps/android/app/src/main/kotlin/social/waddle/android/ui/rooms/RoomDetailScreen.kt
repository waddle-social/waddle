package social.waddle.android.ui.rooms

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import org.koin.compose.koinInject
import social.waddle.android.connection.WaddleConnectionManager
import social.waddle.android.domain.ConversationKey
import social.waddle.android.domain.Timeline
import uniffi.waddle_xmpp_client.WaddleArchivedMessage
import uniffi.waddle_xmpp_client.WaddleMessage
import uniffi.waddle_xmpp_client.WaddleRoom

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RoomDetailScreen(
    room: WaddleRoom,
    ownJid: String,
    onBack: () -> Unit,
) {
    val connection = koinInject<WaddleConnectionManager>()
    val session by connection.activeSession.collectAsState()
    val sessionScope = rememberCoroutineScope()
    val nick = remember(ownJid) { ownJid.substringBefore('@').ifBlank { "me" } }

    LaunchedEffect(room.jid, session) {
        val active = session ?: return@LaunchedEffect
        active.rooms.join(room.jid, nick)
        active.messages.backfill(ConversationKey.Room(room.jid))
    }

    val emptyTimeline = remember { MutableStateFlow(Timeline()) }
    val timeline by (session?.messages?.timeline(ConversationKey.Room(room.jid)) ?: emptyTimeline)
        .collectAsState()

    var draft by remember { mutableStateOf("") }
    val listState = rememberLazyListState()

    LaunchedEffect(timeline.live.size, timeline.archived.size) {
        val total = timeline.live.size + timeline.archived.size
        if (total > 0) listState.animateScrollToItem(total - 1)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("# ${room.name}", style = MaterialTheme.typography.titleMedium)
                        Text(room.jid, style = MaterialTheme.typography.labelSmall)
                    }
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            LazyColumn(
                state = listState,
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(horizontal = 12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(timeline.archived, key = { "mam-${it.mamId}" }) { archived -> ArchivedMessageRow(archived) }
                items(timeline.live, key = { "live-${it.id ?: it.timestamp ?: it.body}" }) { live -> LiveMessageRow(live) }
            }
            Composer(
                value = draft,
                onValueChange = { draft = it },
                onSend = {
                    val body = draft.trim()
                    if (body.isEmpty()) return@Composer
                    draft = ""
                    val active = session ?: return@Composer
                    sessionScope.launch {
                        active.messages.sendRoom(room.jid, body)
                    }
                },
            )
        }
    }
}

@Composable
private fun LiveMessageRow(message: WaddleMessage) {
    val sender = message.from?.substringAfter('/')?.ifBlank { message.from } ?: "(unknown)"
    Column(modifier = Modifier.fillMaxWidth()) {
        Text(text = sender, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold)
        Text(text = message.body.orEmpty(), style = MaterialTheme.typography.bodyLarge)
    }
}

@Composable
private fun ArchivedMessageRow(message: WaddleArchivedMessage) {
    val sender = message.from?.substringAfter('/')?.ifBlank { message.from } ?: "(history)"
    Column(modifier = Modifier.fillMaxWidth()) {
        Text(
            text = sender,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = message.body.orEmpty(),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun Composer(value: String, onValueChange: (String) -> Unit, onSend: () -> Unit) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = Modifier
            .fillMaxWidth()
            .padding(12.dp),
        placeholder = { Text("Message…") },
        trailingIcon = {
            IconButton(onClick = onSend) {
                Icon(Icons.AutoMirrored.Outlined.Send, contentDescription = "Send")
            }
        },
    )
}
