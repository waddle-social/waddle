package social.waddle.android.ui.rooms

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.MutableStateFlow
import org.koin.compose.koinInject
import social.waddle.android.connection.ConnectionState
import social.waddle.android.connection.WaddleConnectionManager
import uniffi.waddle_xmpp_client.WaddleRoom

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RoomListScreen(onRoomSelected: (WaddleRoom) -> Unit) {
    val connection = koinInject<WaddleConnectionManager>()
    val state by connection.state.collectAsState()
    val session by connection.activeSession.collectAsState()
    val rooms by (session?.rooms?.rooms ?: emptyRooms).collectAsState()

    Scaffold(
        topBar = { TopAppBar(title = { Text("Channels") }) },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            verticalArrangement = Arrangement.Top,
        ) {
            ConnectionBanner(state)
            when {
                rooms.isEmpty() && state == ConnectionState.Connected -> EmptyMessage("No channels yet.")
                rooms.isEmpty() -> EmptyMessage("Connecting…")
                else -> LazyColumn {
                    items(rooms, key = { it.jid }) { room ->
                        RoomRow(room, onClick = { onRoomSelected(room) })
                        HorizontalDivider()
                    }
                }
            }
        }
    }
}

@Composable
private fun EmptyMessage(text: String) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ConnectionBanner(state: ConnectionState) {
    val text = when (state) {
        ConnectionState.Connected -> null
        ConnectionState.Connecting -> "Connecting…"
        ConnectionState.Disconnected -> "Offline"
        is ConnectionState.Failed -> "Connection failed: ${state.description}"
    }
    text?.let {
        Text(
            text = it,
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 8.dp),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun RoomRow(room: WaddleRoom, onClick: () -> Unit) {
    ListItem(
        modifier = Modifier.clickable(onClick = onClick),
        headlineContent = { Text("# ${room.name}") },
        supportingContent = {
            val type = room.channelType
            if (type.isNotBlank()) {
                Text(type, style = MaterialTheme.typography.labelSmall)
            }
        },
    )
}

private val emptyRooms = MutableStateFlow<List<WaddleRoom>>(emptyList())
