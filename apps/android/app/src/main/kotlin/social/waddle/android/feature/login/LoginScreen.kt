package social.waddle.android.feature.login

import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.core.net.toUri
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.auth.AuthProvider
import social.waddle.android.theme.JetBrainsMonoFamily
import social.waddle.android.theme.Waddle

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

    LoginContent(
        state = state,
        onSelectProvider = viewModel::selectProvider,
        onCancel = viewModel::cancel,
        onDismissError = viewModel::dismissError,
    )
}

/** Stateless login surface: brand, one line of purpose, then the phase. */
@Composable
internal fun LoginContent(
    state: LoginUiState,
    onSelectProvider: (String) -> Unit,
    onCancel: () -> Unit,
    onDismissError: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .systemBarsPadding()
            .padding(horizontal = Waddle.space.xl, vertical = Waddle.space.xxl),
        verticalArrangement = Arrangement.spacedBy(Waddle.space.md, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Image(
            painter = painterResource(R.drawable.waddle_mark),
            contentDescription = null,
            modifier = Modifier.size(96.dp),
        )
        Text(
            text = stringResource(R.string.app_name),
            style = MaterialTheme.typography.displaySmall,
        )
        Text(
            text = stringResource(R.string.login_subtitle),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(Waddle.space.sm))
        when (state) {
            LoginUiState.LoadingProviders -> CircularProgressIndicator()
            is LoginUiState.ProviderPicker -> ProviderPicker(providers = state.providers, onSelect = onSelectProvider)
            is LoginUiState.Verifying -> Verifying(userCode = state.userCode, onCancel = onCancel)
            LoginUiState.Completing -> Completing()
            is LoginUiState.Failed -> Failed(phase = state, onDismiss = onDismissError)
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
    providers.forEachIndexed { index, provider ->
        // The first provider is the obvious path; the rest stay quieter.
        val content: @Composable RowScope.() -> Unit = {
            Text(
                text = stringResource(
                    R.string.login_continue_with,
                    provider.displayName ?: provider.id,
                ),
                style = MaterialTheme.typography.titleSmall,
            )
        }
        if (index == 0) {
            Button(
                onClick = { onSelect(provider.id) },
                shape = RoundedCornerShape(Waddle.radius.panel),
                modifier = Modifier
                    .fillMaxWidth()
                    .defaultMinSize(minHeight = BUTTON_HEIGHT),
                content = content,
            )
        } else {
            OutlinedButton(
                onClick = { onSelect(provider.id) },
                shape = RoundedCornerShape(Waddle.radius.panel),
                modifier = Modifier
                    .fillMaxWidth()
                    .defaultMinSize(minHeight = BUTTON_HEIGHT),
                content = content,
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
    // The code a person may type on another device: mono, spaced, boxed.
    Surface(
        shape = RoundedCornerShape(Waddle.radius.panel),
        color = MaterialTheme.colorScheme.surfaceContainer,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Text(
            text = userCode,
            style = MaterialTheme.typography.headlineMedium.copy(
                fontFamily = JetBrainsMonoFamily,
                letterSpacing = 0.12.em,
            ),
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(horizontal = Waddle.space.lg, vertical = Waddle.space.sm),
        )
    }
    CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(24.dp))
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
        color = Waddle.colors.dangerText,
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

private val BUTTON_HEIGHT = 52.dp
