package social.waddle.android

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.stateIn
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.launch
import social.waddle.android.client.WaddleAppState
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.SessionPrefs

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
    private val scope: CoroutineScope,
) {
    private val restoreFailure = MutableStateFlow<String?>(null)

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
        restoreFailure.value = null
        val sessionId = sessionPrefs.sessionId.first()
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
    }
}
