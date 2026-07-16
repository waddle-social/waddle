package social.waddle.android.feature.room

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.RoomAdminResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddlePinPermission
import social.waddle.client.ffi.WaddleRoomConfigPatch

/** One-shot events for the settings sheet host. */
sealed interface RoomSettingsEvent {
    data object Saved : RoomSettingsEvent

    /** The room was destroyed: leave the channel screen. */
    data object Destroyed : RoomSettingsEvent

    data class Failed(val result: RoomAdminResult) : RoomSettingsEvent
}

data class RoomSettingsUiState(
    val loading: Boolean = true,
    /** The §10.2 config fetch failed (offline or not owner). */
    val loadFailed: Boolean = false,
    val name: String = "",
    val description: String = "",
    /** Current server-side policy; `null` when the field was not offered. */
    val pinPermission: WaddlePinPermission? = null,
    val saving: Boolean = false,
)

/**
 * Owner room settings: XEP-0045 §10.2 GET (prefill) → edited patch →
 * GET-merge-SET submit, plus the §10.9 destroy. Entry is owner-gated
 * by the channel screen; the server enforces ownership regardless.
 */
class RoomSettingsViewModel(
    private val sessionManager: XmppSessionManager,
    private val roomJid: String,
) : ViewModel() {
    private val _uiState = MutableStateFlow(RoomSettingsUiState())
    val uiState: StateFlow<RoomSettingsUiState> = _uiState.asStateFlow()

    private val _events = MutableSharedFlow<RoomSettingsEvent>(extraBufferCapacity = 4)
    val events: SharedFlow<RoomSettingsEvent> = _events

    init {
        viewModelScope.launch { load() }
    }

    private suspend fun load() {
        val config = sessionManager.fetchRoomConfig(roomJid)
        _uiState.update { state ->
            if (config == null) {
                state.copy(loading = false, loadFailed = true)
            } else {
                state.copy(
                    loading = false,
                    loadFailed = false,
                    name = config.name.orEmpty(),
                    description = config.description.orEmpty(),
                    pinPermission = config.pinPermission,
                )
            }
        }
    }

    fun setName(value: String) = _uiState.update { it.copy(name = value) }

    fun setDescription(value: String) = _uiState.update { it.copy(description = value) }

    fun setPinPermission(value: WaddlePinPermission) =
        _uiState.update { it.copy(pinPermission = value) }

    /**
     * Submit the edit patch. The description is always sent — a
     * present-but-empty `roomdesc` clears it server-side (web parity);
     * the pin permission is sent only when known (never downgrade a
     * policy the fetch could not read).
     */
    fun save() {
        val state = _uiState.value
        val name = state.name.trim()
        if (name.isEmpty() || state.saving) return
        _uiState.update { it.copy(saving = true) }
        viewModelScope.launch {
            val result = sessionManager.submitRoomConfig(
                roomJid,
                WaddleRoomConfigPatch(
                    name = name,
                    description = state.description.trim(),
                    forum = null,
                    pinPermission = state.pinPermission,
                ),
            )
            _uiState.update { it.copy(saving = false) }
            _events.tryEmit(
                if (result == RoomAdminResult.Ok) RoomSettingsEvent.Saved else RoomSettingsEvent.Failed(result),
            )
        }
    }

    /** XEP-0045 §10.9: destroy the room (the host confirms first). */
    fun destroyRoom() {
        if (_uiState.value.saving) return
        _uiState.update { it.copy(saving = true) }
        viewModelScope.launch {
            val result = sessionManager.destroyRoom(roomJid)
            _uiState.update { it.copy(saving = false) }
            _events.tryEmit(
                if (result == RoomAdminResult.Ok) {
                    RoomSettingsEvent.Destroyed
                } else {
                    RoomSettingsEvent.Failed(result)
                },
            )
        }
    }

    companion object {
        fun factory(graph: AppGraph, roomJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                RoomSettingsViewModel(sessionManager = graph.sessionManager, roomJid = roomJid)
            }
    }
}
