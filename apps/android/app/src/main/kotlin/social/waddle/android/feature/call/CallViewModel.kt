package social.waddle.android.feature.call

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.calls.CallKind
import social.waddle.android.client.calls.CallState
import social.waddle.android.viewModelFactoryOf

/**
 * Thin UI adapter over the global call slot + media controller. All
 * call state lives in `core/client`'s CallStore and the app-scoped
 * media session, so this ViewModel only forwards actions and derives
 * presentation state (duration ticker, minimized flag, MUC roster).
 */
class CallViewModel(
    private val sessionManager: XmppSessionManager,
    val media: CallMediaController,
    /**
     * Scope for the WIRE-side actions (accept/decline/hang-up): these
     * must complete even if the activity — and with it viewModelScope —
     * dies mid-send, or the peer is left hanging until their timeout.
     */
    private val actionScope: CoroutineScope,
    liveParticipants: MucCallLiveParticipantsStore = MucCallLiveParticipantsStore(),
) : ViewModel() {
    val callState: StateFlow<CallState> = sessionManager.callStore.state
    val lastError: StateFlow<String?> = sessionManager.callStore.lastError

    private val _minimized = MutableStateFlow(false)

    /** In-call surface collapsed to the banner while the user chats. */
    val minimized: StateFlow<Boolean> = _minimized.asStateFlow()

    private val _durationSeconds = MutableStateFlow(0L)

    /** Seconds since the call turned active; 0 outside `Active`. */
    val durationSeconds: StateFlow<Long> = _durationSeconds.asStateFlow()

    private val presence = sessionManager.callStore.mucCallPresence

    /**
     * Roster of the live MUC call (empty for DM calls): nicks resolved
     * from the LiveKit projection with Muji-presence fallback, plus the
     * `urn:waddle:in-call:0` raised-hand/mute badges.
     */
    val mucRoster: StateFlow<List<MucRosterEntry>> = combine(
        callState,
        combine(
            presence.participants, presence.owners, presence.raisedHands, presence.mutedNicks,
            ::MucPresenceRosterView,
        ),
        combine(liveParticipants.participants, liveParticipants.leavingRooms, ::LiveRosterView),
    ) { state, presenceView, liveView ->
        val room = activeMucRoomOf(state) ?: return@combine emptyList()
        mucRosterOf(room, presenceView, liveView)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(), emptyList())

    /** Whether our own nick advertises the raised-hand marker. */
    val selfHandRaised: StateFlow<Boolean> = combine(
        callState,
        presence.raisedHands,
    ) { state, raised ->
        val room = activeMucRoomOf(state) ?: return@combine false
        val nick = (state as? CallState.Active)?.selfNick ?: return@combine false
        raised[room]?.contains(nick) == true
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(), false)

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
        actionScope.launch { sessionManager.callStore.acceptIncoming() }
    }

    fun decline() {
        actionScope.launch { sessionManager.callStore.declineIncoming() }
    }

    fun hangUp() {
        actionScope.launch { sessionManager.callStore.hangUp() }
    }

    fun dismissEnded() {
        sessionManager.callStore.dismiss()
    }

    fun toggleMic() {
        val enable = !media.micEnabled.value
        viewModelScope.launch { media.setMicEnabled(enable) }
        // Group calls broadcast the mute marker over Muji presence so
        // every occupant's roster shows it (web broadcastMucCallSelfMute).
        if (isActiveMucCall(callState.value)) {
            actionScope.launch { sessionManager.callStore.muc.broadcastSelfMute(muted = !enable) }
        }
    }

    /** Raise/lower our hand in the live MUC call (no-op otherwise). */
    fun toggleHandRaised() {
        if (!isActiveMucCall(callState.value)) return
        val raised = !selfHandRaised.value
        actionScope.launch { sessionManager.callStore.muc.setHandRaised(raised) }
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

        private fun isActiveMucCall(state: CallState): Boolean =
            state is CallState.Active && state.kind == CallKind.MUC

        private fun activeMucRoomOf(state: CallState): String? =
            (state as? CallState.Active)
                ?.takeIf { it.kind == CallKind.MUC }
                ?.let { normalizeCallRoomJid(it.peer) }

        fun factory(graph: AppGraph): ViewModelProvider.Factory = viewModelFactoryOf {
            CallViewModel(
                sessionManager = graph.sessionManager,
                media = graph.callSessionController.media,
                actionScope = graph.applicationScope,
                liveParticipants = graph.mucCallLiveParticipants,
            )
        }
    }
}
