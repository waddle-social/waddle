package social.waddle.android.feature.login

import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.auth.AuthProvider

/**
 * Device-authorization login: pick a provider, approve in a Custom Tab,
 * watch the poll progress (user code shown), errors as text.
 */
@Composable
fun LoginScreen() {
    val graph = LocalAppGraph.current
    val viewModel: LoginViewModel = viewModel(factory = LoginViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val context = LocalContext.current

    LaunchedEffect(viewModel) {
        viewModel.openVerificationUrl.collect { url ->
            // No browser installed is survivable: the user code stays on
            // screen and can be entered on another device.
            runCatching {
                CustomTabsIntent.Builder().build().launchUrl(context, url.toUri())
            }
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(32.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = stringResource(R.string.app_name),
            style = MaterialTheme.typography.headlineLarge,
            color = MaterialTheme.colorScheme.primary,
        )
        Text(
            text = stringResource(R.string.login_subtitle),
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
        )
        when (val phase = state) {
            LoginUiState.LoadingProviders -> CircularProgressIndicator()
            is LoginUiState.ProviderPicker -> ProviderPicker(
                providers = phase.providers,
                onSelect = viewModel::selectProvider,
            )
            is LoginUiState.Verifying -> Verifying(
                userCode = phase.userCode,
                onCancel = viewModel::cancel,
            )
            LoginUiState.Completing -> Completing()
            is LoginUiState.Failed -> Failed(
                phase = phase,
                onDismiss = viewModel::dismissError,
            )
        }
    }
}

@Composable
private fun ProviderPicker(providers: List<AuthProvider>, onSelect: (String) -> Unit) {
    if (providers.isEmpty()) {
        Text(
            text = stringResource(R.string.login_no_providers),
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
        )
        return
    }
    providers.forEach { provider ->
        Button(
            onClick = { onSelect(provider.id) },
            modifier = Modifier
                .fillMaxWidth()
                .defaultMinSize(minHeight = 48.dp),
        ) {
            Text(
                text = stringResource(
                    R.string.login_continue_with,
                    provider.displayName ?: provider.id,
                ),
            )
        }
    }
}

@Composable
private fun Verifying(userCode: String, onCancel: () -> Unit) {
    Text(
        text = stringResource(R.string.login_verification_instructions),
        style = MaterialTheme.typography.bodyMedium,
        textAlign = TextAlign.Center,
    )
    Text(
        text = userCode,
        style = MaterialTheme.typography.headlineMedium,
        color = MaterialTheme.colorScheme.primary,
    )
    CircularProgressIndicator()
    OutlinedButton(
        onClick = onCancel,
        modifier = Modifier.defaultMinSize(minHeight = 48.dp),
    ) {
        Text(text = stringResource(R.string.action_cancel))
    }
}

@Composable
private fun Completing() {
    CircularProgressIndicator()
    Text(
        text = stringResource(R.string.login_completing),
        style = MaterialTheme.typography.bodyMedium,
    )
}

@Composable
private fun Failed(phase: LoginUiState.Failed, onDismiss: () -> Unit) {
    Text(
        text = stringResource(phase.reason.messageRes()),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.error,
        textAlign = TextAlign.Center,
    )
    phase.detail?.let { detail ->
        Text(
            text = detail,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
    Button(
        onClick = onDismiss,
        modifier = Modifier.defaultMinSize(minHeight = 48.dp),
    ) {
        Text(text = stringResource(R.string.action_retry))
    }
}

private fun LoginFailureReason.messageRes(): Int = when (this) {
    LoginFailureReason.PROVIDERS_UNAVAILABLE -> R.string.login_error_providers
    LoginFailureReason.START_FAILED -> R.string.login_error_start
    LoginFailureReason.POLL_FAILED -> R.string.login_error_poll
    LoginFailureReason.EXPIRED -> R.string.login_error_expired
    LoginFailureReason.SESSION_INVALID -> R.string.login_error_session
}
