package social.waddle.android

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import social.waddle.android.client.NetworkSignal
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Cold-start session restore (the `AppModel.bootstrap()` analog): a
 * stored session id is validated via `GET /api/auth/session` — valid ⇒
 * sign in, missing/expired ⇒ signed out, network failure ⇒
 * [WaddleAppState.Error] with a retry hook.
 */
class SessionBootstrap(
    private val sessionPrefs: SessionPrefs,
    private val authApi: WaddleAuthApi,
    private val signIn: suspend (WaddleSessionInfo) -> Unit,
    private val signOutLocally: suspend () -> Unit,
    managerAppState: StateFlow<WaddleAppState>,
    private val networkSignal: NetworkSignal,
    private val scope: CoroutineScope,
) {
    private val restoreFailure = MutableStateFlow<String?>(null)

    private val _splashHold = MutableStateFlow(true)

    /**
     * True only until the LOCAL prefs read completes: the system splash
     * must never be pinned on the session-validation network round-trip
     * (up to the full HTTP timeout on a bad link) — the in-app Loading
     * composable covers that instead.
     */
    val splashHold: StateFlow<Boolean> = _splashHold

    init {
        // Boot-time restores routinely lose the race against Wi-Fi
        // reassociation: the boot receiver has already raised the
        // foreground service, whose resident process would otherwise pin
        // the failed one-shot restore forever (a zombie service that
        // delivers nothing until the user opens the app). Re-run the
        // restore on every connectivity arrival while it is failed —
        // restoreInFlight keeps it single-flight.
        scope.launch {
            networkSignal.online.filter { it }.collect {
                if (restoreFailure.value != null) {
                    restore()
                }
            }
        }
    }

    /**
     * The session-manager app state, with restore failures surfaced as
     * [WaddleAppState.Error] while the manager is still `Loading`.
     */
    val appState: StateFlow<WaddleAppState> =
        combine(managerAppState, restoreFailure) { state, failure ->
            if (failure != null && state is WaddleAppState.Loading) {
                WaddleAppState.Error(failure)
            } else {
                state
            }
        }.stateIn(scope, SharingStarted.Eagerly, WaddleAppState.Loading)

    private val restoreInFlight = AtomicBoolean(false)

    /**
     * (Re-)run the restore; single-flight so a double-tapped retry (or a
     * retry racing the cold-start run) cannot start two concurrent
     * sign-ins and leak a second connection loop.
     */
    fun restore() {
        if (!restoreInFlight.compareAndSet(false, true)) return
        scope.launch {
            try {
                runRestore()
            } finally {
                restoreInFlight.set(false)
            }
        }
    }

    private suspend fun runRestore() {
        // Whole-body guard: DataStore reads/writes (corrupted
        // preferences_pb, disk-full) and login() are not Result-typed
        // like the HTTP call — an uncaught throw on this root coroutine
        // would crash the process at launch instead of surfacing the
        // retryable Error screen.
        try {
            restoreFailure.value = null
            val sessionId = sessionPrefs.sessionId.first()
            _splashHold.value = false
            if (sessionId == null) {
                signOutLocally()
                return
            }
            authApi.session(sessionId).fold(
                onSuccess = { session ->
                    if (session == null || session.isExpired) signOutLocally() else signIn(session)
                },
                onFailure = { failure ->
                    restoreFailure.value = failure.message ?: failure.javaClass.simpleName
                },
            )
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (failure: Throwable) {
            _splashHold.value = false
            restoreFailure.value = failure.message ?: failure.javaClass.simpleName
        }
    }
}
