package social.waddle.android.auth

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.domain.auth.AuthProvider
import social.waddle.android.domain.auth.AuthSession
import social.waddle.android.domain.auth.DevicePollResult
import social.waddle.android.domain.auth.SessionStore
import social.waddle.android.domain.auth.WaddleApiException
import social.waddle.android.domain.auth.WaddleAuthApi

/**
 * Drives the full sign-in lifecycle:
 *
 *  1. On construction restore any persisted server URL + session id.
 *  2. If the cached session is still valid, jump straight to [AuthState.SignedIn].
 *  3. Otherwise load providers and present [AuthState.SignedOut].
 *  4. When the user picks a provider, start RFC 8628 device-code OAuth
 *     and poll until the server reports completion or the user aborts.
 *  5. On sign-out, clear the cached id, reload providers.
 */
internal class AuthViewModel(
    private val api: WaddleAuthApi,
    private val store: SessionStore,
) : ViewModel() {
    private val mutable = MutableStateFlow<AuthState>(AuthState.Bootstrapping)
    val state: StateFlow<AuthState> = mutable.asStateFlow()

    private var pollJob: Job? = null

    init {
        viewModelScope.launch { bootstrap() }
    }

    fun setServerUrl(value: String) {
        val current = mutable.value as? AuthState.SignedOut ?: return
        mutable.value = current.copy(serverUrl = value)
    }

    fun retryProviders() {
        viewModelScope.launch {
            val current = mutable.value as? AuthState.SignedOut ?: return@launch
            loadProviders(current.serverUrl)
        }
    }

    fun signIn(provider: AuthProvider) {
        viewModelScope.launch {
            val current = mutable.value as? AuthState.SignedOut ?: return@launch
            val serverUrl = current.serverUrl
            try {
                store.saveServerUrl(serverUrl)
                val flow = api.startDeviceAuth(serverUrl, provider.id)
                mutable.value = AuthState.AwaitingDevice(serverUrl, provider, flow)
                startPolling(flow.deviceCode, flow.interval, serverUrl)
            } catch (error: Throwable) {
                mutable.value = current.copy(errorMessage = error.userMessage())
            }
        }
    }

    fun cancelDeviceFlow() {
        pollJob?.cancel()
        pollJob = null
        val current = mutable.value as? AuthState.AwaitingDevice ?: return
        viewModelScope.launch {
            mutable.value = AuthState.SignedOut(current.serverUrl, providersOrEmpty(current.serverUrl))
        }
    }

    fun signOut() {
        viewModelScope.launch {
            val current = mutable.value as? AuthState.SignedIn ?: return@launch
            pollJob?.cancel()
            runCatching { api.logout(current.serverUrl, current.session.sessionId) }
            store.clearSessionId()
            mutable.value = AuthState.SignedOut(current.serverUrl, providersOrEmpty(current.serverUrl))
        }
    }

    private suspend fun bootstrap() {
        // Wrap the whole bootstrap so a DataStore init failure or network
        // hiccup never escapes — uncaught throws here would tear down the
        // ViewModel and the entire UI before the user sees anything.
        val serverUrl = runCatching { store.currentServerUrl() }
            .getOrElse { SessionStore.DEFAULT_SERVER_URL }
        val sessionId = runCatching { store.currentSessionId() }.getOrNull()
        if (sessionId != null) {
            try {
                val session = api.session(serverUrl, sessionId)
                if (session != null && !session.isExpired) {
                    mutable.value = AuthState.SignedIn(serverUrl, session)
                    return
                }
                runCatching { store.clearSessionId() }
            } catch (_: Throwable) {
                // Network failure on bootstrap → fall back to sign-in flow.
            }
        }
        loadProviders(serverUrl)
    }

    private suspend fun loadProviders(serverUrl: String) {
        mutable.value = AuthState.SignedOut(serverUrl, emptyList(), isLoadingProviders = true)
        val providers = providersOrEmpty(serverUrl)
        mutable.value = AuthState.SignedOut(serverUrl, providers, isLoadingProviders = false)
    }

    private suspend fun providersOrEmpty(serverUrl: String): List<AuthProvider> =
        runCatching { api.providers(serverUrl) }.getOrDefault(emptyList())

    private fun startPolling(deviceCode: String, intervalSeconds: Int, serverUrl: String) {
        pollJob?.cancel()
        pollJob = viewModelScope.launch {
            val delayMs = intervalSeconds.coerceAtLeast(1) * 1000L
            while (true) {
                delay(delayMs)
                val result = runCatching { api.pollDeviceAuth(serverUrl, deviceCode) }
                    .getOrElse { error ->
                        // 400 slow_down etc — keep polling but surface the message.
                        if (error is WaddleApiException && error.statusCode in 400..499) {
                            DevicePollResult.Pending
                        } else {
                            updateAwaiting { it.copy(errorMessage = error.userMessage()) }
                            return@launch
                        }
                    }
                if (result is DevicePollResult.Complete) {
                    onSessionAccepted(serverUrl, result.sessionId)
                    return@launch
                }
            }
        }
    }

    private suspend fun onSessionAccepted(serverUrl: String, sessionId: String) {
        try {
            val session = api.session(serverUrl, sessionId)
                ?: throw WaddleApiException(0, "session not found after device-flow completion")
            store.saveSessionId(sessionId)
            mutable.value = AuthState.SignedIn(serverUrl, session)
        } catch (error: Throwable) {
            updateAwaiting { it.copy(errorMessage = error.userMessage()) }
        }
    }

    private fun updateAwaiting(transform: (AuthState.AwaitingDevice) -> AuthState.AwaitingDevice) {
        val current = mutable.value as? AuthState.AwaitingDevice ?: return
        mutable.value = transform(current)
    }

    private fun Throwable.userMessage(): String = when (this) {
        is WaddleApiException -> if (statusCode > 0) "HTTP $statusCode: $detail" else detail
        else -> message ?: this::class.simpleName ?: "Unknown error"
    }
}
