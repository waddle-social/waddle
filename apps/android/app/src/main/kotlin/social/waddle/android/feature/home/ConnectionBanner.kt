package social.waddle.android.feature.home

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.ConnectionState

/** Persistent surface while the connection is not ready. */
@Composable
fun ConnectionBanner(
    state: ConnectionState,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val text = when (state) {
        ConnectionState.Connecting -> stringResource(R.string.connection_connecting)
        is ConnectionState.Reconnecting ->
            stringResource(R.string.connection_reconnecting, state.attempt)
        ConnectionState.Offline -> stringResource(R.string.connection_offline)
        ConnectionState.AuthFailed -> stringResource(R.string.connection_auth_failed)
        ConnectionState.Failed -> stringResource(R.string.connection_failed)
        ConnectionState.Idle, ConnectionState.Ready -> null
    } ?: return

    Surface(color = MaterialTheme.colorScheme.tertiaryContainer, modifier = modifier) {
        Row(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
            Text(
                text = text,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onTertiaryContainer,
                modifier = Modifier
                    .weight(1f)
                    .padding(vertical = 4.dp)
                    .semantics { liveRegion = LiveRegionMode.Polite },
            )
            // Server-side outages never produce an offline->online edge,
            // so the parked loop needs a manual trigger.
            if (state == ConnectionState.Failed) {
                TextButton(onClick = onRetry) {
                    Text(text = stringResource(R.string.connection_retry))
                }
            }
        }
    }
}
