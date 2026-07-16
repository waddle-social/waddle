package social.waddle.android.feature.call

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.calls.CallState
import social.waddle.android.viewModelFactoryOf

/**
 * Thin UI adapter over the global call slot + media controller. All
 * call state lives in `core/client`'s CallStore and the app-scoped
 * media session, so this ViewModel only forwards actions and derives
 * presentation state (duration ticker, minimized flag).
 */
class CallViewModel(
    private val sessionManager: XmppSessionManager,
    val media: CallMediaController,
) : ViewModel() {
    val callState: StateFlow<CallState> = sessionManager.callStore.state
    val lastError: StateFlow<String?> = sessionManager.callStore.lastError

    private val _minimized = MutableStateFlow(false)

    /** In-call surface collapsed to the banner while the user chats. */
    val minimized: StateFlow<Boolean> = _minimized.asStateFlow()

    private val _durationSeconds = MutableStateFlow(0L)

    /** Seconds since the call turned active; 0 outside `Active`. */
    val durationSeconds: StateFlow<Long> = _durationSeconds.asStateFlow()

    init {
        viewModelScope.launch {
            callState.collectLatest { state ->
                when (state) {
                    is CallState.Active -> {
                        var seconds = 0L
                        while (true) {
                            _durationSeconds.value = seconds
                            delay(DURATION_TICK_MILLIS)
                            seconds += 1
                        }
                    }
                    else -> {
                        _durationSeconds.value = 0
                        // A fresh call always starts expanded.
                        if (state is CallState.Idle || state is CallState.Incoming) {
                            _minimized.value = false
                        }
                    }
                }
            }
        }
    }

    fun accept() {
        viewModelScope.launch { sessionManager.callStore.acceptIncoming() }
    }

    fun decline() {
        viewModelScope.launch { sessionManager.callStore.declineIncoming() }
    }

    fun hangUp() {
        viewModelScope.launch { sessionManager.callStore.hangUp() }
    }

    fun dismissEnded() {
        sessionManager.callStore.dismiss()
    }

    fun toggleMic() {
        viewModelScope.launch { media.setMicEnabled(!media.micEnabled.value) }
    }

    fun toggleCamera() {
        viewModelScope.launch { media.setCameraEnabled(!media.cameraEnabled.value) }
    }

    fun toggleSpeaker() {
        media.setSpeakerphone(!media.speakerOn.value)
    }

    fun flipCamera() {
        viewModelScope.launch { media.flipCamera() }
    }

    fun setMinimized(minimized: Boolean) {
        _minimized.value = minimized
    }

    companion object {
        private const val DURATION_TICK_MILLIS = 1_000L

        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            CallViewModel(
                sessionManager = graph.sessionManager,
                media = graph.callSessionController.media,
            )
        }
    }
}
