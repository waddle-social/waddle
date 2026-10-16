package social.waddle.android.feature.home

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.client.ConnectionState
import social.waddle.android.theme.Waddle

/**
 * Quiet strip while the connection is not ready: a spinner while it
 * works on it, a glyph when it is waiting on the network or the user.
 * Never alarm-coloured: reconnecting is routine.
 */
@Composable
fun ConnectionBanner(
    state: ConnectionState,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val text = when (state) {
        ConnectionState.Connecting -> stringResource(R.string.connection_connecting)
        is ConnectionState.Reconnecting -> stringResource(R.string.connection_reconnecting, state.attempt)
        ConnectionState.Offline -> stringResource(R.string.connection_offline)
        ConnectionState.AuthFailed -> stringResource(R.string.connection_auth_failed)
        ConnectionState.Idle, ConnectionState.Ready -> null
    }
    AnimatedVisibility(
        visible = text != null,
        enter = expandVertically() + fadeIn(),
        exit = shrinkVertically() + fadeOut(),
        modifier = modifier,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xxs)
                .background(MaterialTheme.colorScheme.surfaceContainerHigh, RoundedCornerShape(Waddle.radius.lg))
                .padding(start = Waddle.space.sm, end = Waddle.space.xxs, top = 2.dp, bottom = 2.dp)
                .semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
        ) {
            when (state) {
                ConnectionState.Offline -> StatusIcon { Icon(Icons.Outlined.CloudOff, null, Modifier.size(16.dp)) }
                ConnectionState.AuthFailed -> StatusIcon {
                    Icon(Icons.Outlined.ErrorOutline, null, Modifier.size(16.dp), tint = Waddle.colors.dangerText)
                }
                else -> CircularProgressIndicator(
                    strokeWidth = 2.dp,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(14.dp),
                )
            }
            Text(
                text = text.orEmpty(),
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .weight(1f)
                    .padding(vertical = Waddle.space.xs),
            )
            // Backoff can reach a minute; let the user skip the wait.
            if (state is ConnectionState.Reconnecting) {
                TextButton(onClick = onRetry) {
                    Text(text = stringResource(R.string.connection_retry))
                }
            }
        }
    }
}

@Composable
private fun StatusIcon(icon: @Composable () -> Unit) {
    CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurfaceVariant) { icon() }
}
