package social.waddle.android.ui.rooms

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.ExperimentalMaterial3Api
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
import kotlinx.coroutines.flow.StateFlow
import org.koin.compose.koinInject
import social.waddle.android.connection.WaddleConnectionManager
import social.waddle.android.connection.ConnectionState
import uniffi.waddle_xmpp_client.WaddleRoom

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RoomListScreen() {
    val connection = koinInject<WaddleConnectionManager>()
    val state by connection.state.collectAsState()
    val rooms by emptyRoomsFlow.collectAsState()

    Scaffold(
        topBar = {
            TopAppBar(title = { Text("Channels") })
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            verticalArrangement = Arrangement.Top,
        ) {
            ConnectionBanner(state)
            if (rooms.isEmpty()) {
                Box(
                    modifier = Modifier.fillMaxSize().padding(24.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = "Sign in to load your channels.",
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else {
                LazyColumn(
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(
                        horizontal = 8.dp,
                        vertical = 4.dp,
                    ),
                ) {
                    items(rooms, key = { it.jid }) { room -> RoomRow(room) }
                }
            }
        }
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
                .fillMaxSize()
                .padding(horizontal = 16.dp, vertical = 8.dp),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun RoomRow(room: WaddleRoom) {
    androidx.compose.material3.ListItem(
        headlineContent = { Text(room.name) },
        supportingContent = { Text(room.channelType, style = MaterialTheme.typography.labelSmall) },
    )
}

// Until a real RoomsViewModel arrives, the room list is fed by a placeholder
// flow. Hooking the screen up to `domain.RoomRepository` is the next step.
private val emptyRoomsFlow: StateFlow<List<WaddleRoom>> = MutableStateFlow(emptyList())
