package social.waddle.android.feature.login

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.auth.AuthProvider
import social.waddle.android.client.auth.DevicePollResult
import social.waddle.android.client.auth.DeviceStartResponse
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.auth.isTransientDeviceAuthPollError
import social.waddle.android.viewModelFactoryOf

/** The login screen's phase machine. */
sealed interface LoginUiState {
    data object LoadingProviders : LoginUiState

    data class ProviderPicker(val providers: List<AuthProvider>) : LoginUiState

    /** Device flow started; polling while the user approves in the browser. */
    data class Verifying(val userCode: String) : LoginUiState

    /** Poll completed; fetching the session and starting XMPP. */
    data object Completing : LoginUiState

    data class Failed(val reason: LoginFailureReason, val detail: String?) : LoginUiState
}

enum class LoginFailureReason {
    PROVIDERS_UNAVAILABLE,
    START_FAILED,
    POLL_FAILED,
    EXPIRED,
    SESSION_INVALID,
}

/**
 * Device-authorization login (Apple `AppModel+Auth` parity): provider
 * list → device/start → Custom Tab at the verification URL → poll per
 * `interval` (transient IO errors retry) → session fetch → [signIn].
 */
class LoginViewModel(
    private val gateway: LoginAuthGateway,
    private val signIn: suspend (WaddleSessionInfo) -> Unit,
) : ViewModel() {
    private val _uiState = MutableStateFlow<LoginUiState>(LoginUiState.LoadingProviders)
    val uiState: StateFlow<LoginUiState> = _uiState.asStateFlow()

    private val openUrl = Channel<String>(Channel.BUFFERED)

    /** One-shot verification URLs the screen opens in a Custom Tab. */
    val openVerificationUrl: Flow<String> = openUrl.receiveAsFlow()

    private var cachedProviders: List<AuthProvider> = emptyList()
    private var flowJob: Job? = null

    init {
        loadProviders()
    }

    fun loadProviders() {
        flowJob?.cancel()
        flowJob = viewModelScope.launch {
            _uiState.value = LoginUiState.LoadingProviders
            gateway.providers().fold(
                onSuccess = { providers ->
                    cachedProviders = providers
                    _uiState.value = LoginUiState.ProviderPicker(providers)
                },
                onFailure = { failure ->
                    fail(LoginFailureReason.PROVIDERS_UNAVAILABLE, failure)
                },
            )
        }
    }

    fun selectProvider(providerId: String) {
        flowJob?.cancel()
        flowJob = viewModelScope.launch { runDeviceFlow(providerId) }
    }

    /** Abort a running device flow and return to the provider list. */
    fun cancel() {
        flowJob?.cancel()
        flowJob = null
        backToPicker()
    }

    fun dismissError() {
        backToPicker()
    }

    private fun backToPicker() {
        if (cachedProviders.isEmpty()) {
            loadProviders()
        } else {
            _uiState.value = LoginUiState.ProviderPicker(cachedProviders)
        }
    }

    private suspend fun runDeviceFlow(providerId: String) {
        val start = gateway.startDeviceAuth(providerId).getOrElse { failure ->
            fail(LoginFailureReason.START_FAILED, failure)
            return
        }
        verificationUrlOf(start)?.let { openUrl.trySend(it) }
        _uiState.value = LoginUiState.Verifying(start.userCode)
        val sessionId = pollUntilComplete(start) ?: return
        _uiState.value = LoginUiState.Completing
        val session = gateway.session(sessionId).getOrElse { failure ->
            fail(LoginFailureReason.SESSION_INVALID, failure)
            return
        }
        if (session == null || session.isExpired) {
            fail(LoginFailureReason.SESSION_INVALID, null)
            return
        }
        // login() persists to DataStore; a disk failure must surface as a
        // retryable login error, not crash the process.
        try {
            signIn(session)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (failure: Throwable) {
            fail(LoginFailureReason.SESSION_INVALID, failure)
        }
    }

    /** `null` return means the flow already failed (state is set). */
    private suspend fun pollUntilComplete(start: DeviceStartResponse): String? {
        val intervalMillis = start.interval.coerceAtLeast(1) * MILLIS_PER_SECOND
        val budgetMillis = start.expiresIn?.let { it * MILLIS_PER_SECOND } ?: DEFAULT_EXPIRY_MILLIS
        var elapsedMillis = 0L
        while (true) {
            delay(intervalMillis)
            elapsedMillis += intervalMillis
            if (elapsedMillis >= budgetMillis) {
                fail(LoginFailureReason.EXPIRED, null)
                return null
            }
            val result = gateway.pollDeviceAuth(start.deviceCode)
            val failure = result.exceptionOrNull()
            if (failure != null) {
                if (isTransientDeviceAuthPollError(failure)) continue
                fail(LoginFailureReason.POLL_FAILED, failure)
                return null
            }
            when (val poll = result.getOrThrow()) {
                DevicePollResult.Pending -> Unit
                is DevicePollResult.Complete -> return poll.sessionId
            }
        }
    }

    private fun fail(reason: LoginFailureReason, failure: Throwable?) {
        _uiState.value = LoginUiState.Failed(reason, failure?.message)
    }

    private fun verificationUrlOf(start: DeviceStartResponse): String? =
        start.verificationUriComplete ?: start.verificationUri

    companion object {
        private const val MILLIS_PER_SECOND = 1_000L

        /** Server sends `expires_in`; this is only the absent-field fallback. */
        private const val DEFAULT_EXPIRY_MILLIS = 10 * 60 * MILLIS_PER_SECOND

        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            LoginViewModel(
                gateway = WaddleLoginAuthGateway(graph.authApi),
                signIn = graph::signIn,
            )
        }
    }
}
