package social.waddle.android.ui.profile

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
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
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import org.koin.compose.koinInject
import social.waddle.android.R
import social.waddle.android.connection.ConnectionState
import social.waddle.android.connection.WaddleConnectionManager

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ProfileScreen() {
    val connection = koinInject<WaddleConnectionManager>()
    val state by connection.state.collectAsState()

    Scaffold(
        topBar = { TopAppBar(title = { Text("You") }) },
    ) { padding ->
        Column(
            modifier = Modifier.fillMaxSize().padding(padding).padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Image(
                painter = painterResource(R.drawable.waddle_logo),
                contentDescription = null,
                modifier = Modifier.size(96.dp),
            )
            Text(
                text = "Waddle",
                style = MaterialTheme.typography.headlineMedium,
            )
            Text(
                text = state.label(),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

private fun ConnectionState.label(): String = when (this) {
    ConnectionState.Connected -> "Connected"
    ConnectionState.Connecting -> "Connecting…"
    ConnectionState.Disconnected -> "Not connected"
    is ConnectionState.Failed -> "Failed: $description"
}
