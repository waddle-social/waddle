package social.waddle.android.ui.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import org.koin.androidx.compose.koinViewModel
import social.waddle.android.auth.AuthState
import social.waddle.android.auth.AuthViewModel

/**
 * Top-level auth gate. Decides between the bootstrap spinner, the
 * provider-picker, the device-code wait, and the signed-in app shell.
 *
 * The signed-in shell receives the active session so callers can build
 * a `WaddleConfig` and ignite the connection manager.
 */
@Composable
internal fun AuthGate(
    authenticated: @Composable (state: AuthState.SignedIn, onSignOut: () -> Unit) -> Unit,
) {
    val viewModel: AuthViewModel = koinViewModel()
    val state by viewModel.state.collectAsState()

    when (val current = state) {
        AuthState.Bootstrapping -> Bootstrapping()

        is AuthState.SignedOut -> SignInScreen(
            state = current,
            onServerUrlChange = viewModel::setServerUrl,
            onRetryProviders = viewModel::retryProviders,
            onSelectProvider = viewModel::signIn,
        )

        is AuthState.AwaitingDevice -> DeviceCodeScreen(
            state = current,
            onCancel = viewModel::cancelDeviceFlow,
        )

        is AuthState.SignedIn -> authenticated(current, viewModel::signOut)
    }
}

@Composable
private fun Bootstrapping() {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(
            verticalArrangement = Arrangement.spacedBy(16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.padding(24.dp),
        ) {
            CircularProgressIndicator(modifier = Modifier.size(40.dp))
            Text(
                text = "Restoring your session…",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
