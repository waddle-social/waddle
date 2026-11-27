package social.waddle.android.feature.login

import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.core.net.toUri
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.auth.AuthProvider
import social.waddle.android.client.server.ServerDiscoveryError
import social.waddle.android.theme.MonoFamily
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.HuddleFieldShape
import social.waddle.android.ui.Wordmark
import social.waddle.android.ui.huddleFieldColors
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone

/**
 * Device-authorization login: pick a provider, approve in a Custom Tab,
 * watch the poll progress (user code shown), errors as text.
 */
@Composable
fun LoginScreen() {
    val graph = LocalAppGraph.current
    val viewModel: LoginViewModel = viewModel(factory = LoginViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val server by viewModel.server.collectAsStateWithLifecycle()
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
        server = server,
        serverActions = ServerActions(
            onEdit = viewModel::editServer,
            onCancel = viewModel::cancelServerEdit,
            onSubmit = viewModel::submitServer,
            onUseDefault = viewModel::useDefaultServer,
        ),
    )
}

/**
 * Stateless login surface: the mark on its tile, the `waddle.`
 * wordmark, one line about what Waddle is for, then the phase.
 */
@Composable
internal fun LoginContent(
    state: LoginUiState,
    onSelectProvider: (String) -> Unit,
    onCancel: () -> Unit,
    onDismissError: () -> Unit,
    /** `null` hides the server row (previews and golden captures). */
    server: ServerUiState? = null,
    serverActions: ServerActions = ServerActions(),
) {
    val colors = Waddle.colors
    BoxWithConstraints(
        modifier = Modifier
            .fillMaxSize()
            .background(colors.bg)
            .systemBarsPadding(),
    ) {
        // Centred when it fits, scrollable when the font scale makes it tall.
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .heightIn(min = maxHeight)
                .padding(horizontal = Waddle.space.xl, vertical = Waddle.space.xl),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            LoginBody(
                state = state,
                onSelectProvider = onSelectProvider,
                onCancel = onCancel,
                onDismissError = onDismissError,
                server = server,
                serverActions = serverActions,
            )
        }
    }
}

@Composable
private fun LoginBody(
    state: LoginUiState,
    onSelectProvider: (String) -> Unit,
    onCancel: () -> Unit,
    onDismissError: () -> Unit,
    server: ServerUiState?,
    serverActions: ServerActions,
) {
    val colors = Waddle.colors
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        MarkTile()
        Wordmark(
            style = Waddle.type.display,
            modifier = Modifier
                .padding(top = Waddle.space.lg)
                .semantics { heading() },
        )
        Text(
            text = stringResource(R.string.huddle_login_lead),
            style = Waddle.type.lead,
            color = colors.fgMuted,
            textAlign = TextAlign.Center,
            modifier = Modifier
                .widthIn(max = LEAD_WIDTH)
                .padding(top = Waddle.space.xs),
        )
        Spacer(Modifier.height(Waddle.space.xl))
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier.widthIn(max = ACTIONS_WIDTH),
        ) {
            when (state) {
                LoginUiState.LoadingProviders -> Working()
                is LoginUiState.ProviderPicker -> ProviderPicker(
                    providers = state.providers,
                    onSelect = onSelectProvider,
                )
                is LoginUiState.Verifying -> Verifying(userCode = state.userCode, onCancel = onCancel)
                LoginUiState.Completing -> Working(label = stringResource(R.string.login_completing))
                is LoginUiState.Failed -> Failed(phase = state, onDismiss = onDismissError)
            }
            // Mid-flow (code shown / completing) the server is fixed.
            val serverEditable = state !is LoginUiState.Verifying && state != LoginUiState.Completing
            if (server != null && serverEditable) {
                ServerRow(server = server, actions = serverActions)
            }
        }
    }
}

/**
 * The penguin mark on a teal-washed tile: the mark's own dark bubble
 * would sink into the night ground without it.
 */
@Composable
private fun MarkTile() {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(MARK_TILE_RADIUS)
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .size(MARK_TILE)
            .background(colors.actionSubtle, shape)
            .border(BorderStroke(1.dp, colors.border), shape),
    ) {
        Image(
            painter = painterResource(R.drawable.waddle_mark),
            contentDescription = null,
            modifier = Modifier.size(MARK),
        )
    }
}

@Composable
private fun ProviderPicker(providers: List<AuthProvider>, onSelect: (String) -> Unit) {
    val colors = Waddle.colors
    if (providers.isEmpty()) {
        Text(
            text = stringResource(R.string.login_no_providers),
            style = Waddle.type.body,
            color = colors.fgMuted,
            textAlign = TextAlign.Center,
        )
        return
    }
    Text(
        text = stringResource(R.string.login_subtitle),
        style = Waddle.type.control,
        color = colors.fgMuted,
        textAlign = TextAlign.Center,
        modifier = Modifier.padding(bottom = Waddle.space.xxs),
    )
    providers.forEachIndexed { index, provider ->
        // The first provider is the obvious path; the rest are quiet but
        // never read as disabled.
        PillButton(
            text = stringResource(R.string.login_continue_with, provider.displayName ?: provider.id),
            onClick = { onSelect(provider.id) },
            tone = if (index == 0) PillTone.PRIMARY else PillTone.QUIET,
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = Waddle.control.touch),
        )
    }
}

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
private fun Verifying(userCode: String, onCancel: () -> Unit) {
    val colors = Waddle.colors
    Text(
        text = stringResource(R.string.login_verification_instructions),
        style = Waddle.type.body,
        color = colors.fgMuted,
        textAlign = TextAlign.Center,
    )
    // The code a person may type on another device: mono, spaced, boxed.
    Surface(
        shape = RoundedCornerShape(Waddle.radius.card),
        color = colors.surface,
        border = BorderStroke(1.dp, colors.border),
    ) {
        Text(
            text = userCode,
            style = MaterialTheme.typography.headlineMedium.copy(
                fontFamily = MonoFamily,
                letterSpacing = 0.12.em,
            ),
            color = colors.actionText,
            modifier = Modifier.padding(horizontal = Waddle.space.lg, vertical = Waddle.space.sm),
        )
    }
    LoadingIndicator(color = colors.action, modifier = Modifier.size(Waddle.space.xxl))
    PillButton(
        text = stringResource(R.string.action_cancel),
        onClick = onCancel,
        tone = PillTone.QUIET,
    )
}

/** An expressive loading indicator, with what is happening when known. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
private fun Working(label: String? = null) {
    LoadingIndicator(color = Waddle.colors.action, modifier = Modifier.size(Waddle.space.xxl))
    label?.let {
        Text(text = it, style = Waddle.type.body, color = Waddle.colors.fgMuted)
    }
}

@Composable
private fun Failed(phase: LoginUiState.Failed, onDismiss: () -> Unit) {
    val colors = Waddle.colors
    Text(
        text = stringResource(phase.reason.messageRes()),
        style = Waddle.type.body,
        color = colors.dangerText,
        textAlign = TextAlign.Center,
    )
    phase.detail?.let { detail ->
        Text(
            text = detail,
            style = Waddle.type.time,
            color = colors.fgMuted,
            textAlign = TextAlign.Center,
            maxLines = DETAIL_LINES,
        )
    }
    PillButton(
        text = stringResource(R.string.action_retry),
        onClick = onDismiss,
        modifier = Modifier.heightIn(min = Waddle.control.touch),
    )
}

/** Callbacks of the server row; defaults are no-ops for previews. */
internal data class ServerActions(
    val onEdit: () -> Unit = {},
    val onCancel: () -> Unit = {},
    val onSubmit: (String) -> Unit = {},
    val onUseDefault: () -> Unit = {},
)

object LoginTestTags {
    const val SERVER_CHANGE = "login-server-change"
    const val SERVER_FIELD = "login-server-field"
    const val SERVER_CONNECT = "login-server-connect"
}

/**
 * Which server sign-in talks to: a quiet line with the host and a
 * Change action, expanding into an address field. Nothing is saved
 * until the server answers as Waddle.
 */
@Composable
private fun ServerRow(server: ServerUiState, actions: ServerActions) {
    val colors = Waddle.colors
    if (!server.editing) {
        Text(
            text = stringResource(R.string.login_server_current, server.label),
            style = Waddle.type.time,
            color = colors.fgMuted,
            textAlign = TextAlign.Center,
            modifier = Modifier.padding(top = Waddle.space.md),
        )
        PillButton(
            text = stringResource(R.string.login_server_change),
            onClick = actions.onEdit,
            tone = PillTone.QUIET,
            modifier = Modifier
                .heightIn(min = Waddle.control.touch)
                .testTag(LoginTestTags.SERVER_CHANGE),
        )
        if (server.isCustom) {
            PillButton(
                text = stringResource(R.string.login_server_use_default),
                onClick = actions.onUseDefault,
                tone = PillTone.QUIET,
                modifier = Modifier.heightIn(min = Waddle.control.touch),
            )
        }
        return
    }
    var address by rememberSaveable { mutableStateOf("") }
    val submit = { if (address.isNotBlank() && !server.checking) actions.onSubmit(address) }
    OutlinedTextField(
        value = address,
        onValueChange = { address = it },
        label = { Text(text = stringResource(R.string.login_server_label)) },
        placeholder = { Text(text = stringResource(R.string.login_server_placeholder)) },
        singleLine = true,
        isError = server.error != null,
        supportingText = server.error?.let { { Text(text = stringResource(it.messageRes())) } },
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Go),
        keyboardActions = KeyboardActions(onGo = { submit() }),
        shape = HuddleFieldShape,
        colors = huddleFieldColors(),
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = Waddle.space.md)
            .testTag(LoginTestTags.SERVER_FIELD),
    )
    if (server.checking) {
        Working(label = stringResource(R.string.login_server_checking))
    } else {
        PillButton(
            text = stringResource(R.string.login_server_connect),
            onClick = submit,
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = Waddle.control.touch)
                .testTag(LoginTestTags.SERVER_CONNECT),
        )
    }
    PillButton(
        text = stringResource(R.string.action_cancel),
        onClick = actions.onCancel,
        tone = PillTone.QUIET,
    )
}

private fun ServerDiscoveryError.messageRes(): Int = when (this) {
    ServerDiscoveryError.INVALID_ADDRESS -> R.string.login_server_error_invalid
    ServerDiscoveryError.INSECURE_SCHEME -> R.string.login_server_error_insecure
    ServerDiscoveryError.UNREACHABLE -> R.string.login_server_error_unreachable
    ServerDiscoveryError.NOT_A_WADDLE_SERVER -> R.string.login_server_error_not_waddle
}

private fun LoginFailureReason.messageRes(): Int = when (this) {
    LoginFailureReason.PROVIDERS_UNAVAILABLE -> R.string.login_error_providers
    LoginFailureReason.START_FAILED -> R.string.login_error_start
    LoginFailureReason.POLL_FAILED -> R.string.login_error_poll
    LoginFailureReason.EXPIRED -> R.string.login_error_expired
    LoginFailureReason.SESSION_INVALID -> R.string.login_error_session
}

private const val DETAIL_LINES = 3
private val MARK_TILE = 104.dp
private val MARK_TILE_RADIUS = 28.dp
private val MARK = 76.dp
private val LEAD_WIDTH = 300.dp
private val ACTIONS_WIDTH = 360.dp
