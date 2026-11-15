package social.waddle.android.feature.events

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.client.XmppSessionManager
import social.waddle.client.ffi.WaddleBookmarkItem
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddlePlanningItem
import social.waddle.client.ffi.WaddlePlanningKind

internal data class PlanningState(
    val rooms: List<WaddleBookmarkItem> = emptyList(),
    val room: String? = null,
    val items: List<WaddlePlanningItem> = emptyList(),
    val busy: Boolean = false,
    val failed: Boolean = false,
)

internal class EventPlanningModel(
    private val manager: XmppSessionManager,
    private val selfJid: String,
) : ViewModel() {
    private val _state = MutableStateFlow(PlanningState())
    val state = _state.asStateFlow()
    private var operationJob: Job? = null
    private var pendingCreateRoom: String? = null
    init { refresh() }

    fun refresh() = operation {
        val rooms = manager.fetchEventRooms() ?: return@operation false
        val room = _state.value.room
        val items = if (room != null) manager.eventPlanningItems(room) ?: return@operation false else emptyList()
        _state.value = _state.value.copy(rooms = rooms, items = items)
        true
    }

    fun open(room: String) = operation {
        val joined = manager.ensureRoomJoined(room, selfJid.substringBefore('@'))
        if (joined != social.waddle.android.client.VerbResult.Ok) {
            return@operation false
        }
        val items = manager.eventPlanningItems(room) ?: return@operation false
        val bookmarks = manager.fetchEventRooms()
        if (bookmarks != null && bookmarks.none { it.jid == room }) {
            manager.setRoomNotificationMode(
                room, WaddleNotifyMode.ON_MENTION, items.firstOrNull { it.id == "event" }?.title,
            )
        }
        _state.value = _state.value.copy(room = room, items = items)
        true
    }

    fun close() {
        cancelPending()
        pendingCreateRoom = null
        _state.value = _state.value.copy(room = null, items = emptyList())
        refresh()
    }

    fun create(metadata: WaddlePlanningItem, onSaved: () -> Unit) = operation {
        val room = pendingCreateRoom ?: manager.createEventRoom(metadata.title, true) ?: return@operation false
        pendingCreateRoom = room
        _state.value = _state.value.copy(room = room)
        val saved = manager.eventPlanningPublish(room, metadata.copy(id = "event", kind = WaddlePlanningKind.EVENT))
            ?: return@operation false
        _state.value = _state.value.copy(room = room, items = listOf(saved))
        pendingCreateRoom = null
        onSaved()
        true
    }

    fun save(item: WaddlePlanningItem, onSaved: () -> Unit = {}) = operation {
        val room = _state.value.room ?: return@operation false
        val input = if (item.kind == WaddlePlanningKind.EVENT) item.copy(id = "event") else item
        val saved = manager.eventPlanningPublish(room, input) ?: return@operation false
        _state.value = _state.value.copy(items = _state.value.items.filter { it.id != saved.id } + saved)
        onSaved()
        true
    }

    fun invite(target: String, coOrganiser: Boolean, onInvited: () -> Unit) = operation {
        val room = _state.value.room ?: return@operation false
        val affiliation = if (coOrganiser) {
            social.waddle.client.ffi.WaddleMucAffiliation.ADMIN
        } else {
            social.waddle.client.ffi.WaddleMucAffiliation.MEMBER
        }
        if (manager.setRoomAffiliation(room, target, affiliation) != social.waddle.android.client.RoomAdminResult.Ok) {
            return@operation false
        }
        val title = _state.value.items.firstOrNull { it.id == "event" }?.title ?: room
        val message = manager.sendChatMessage(
            target, "You are invited to $title. Open this event room in Waddle: $room",
        )
        val accepted = message.queued || message.outcome is social.waddle.client.ffi.WaddleSendMessageOutcome.Sent
        if (accepted) onInvited()
        accepted
    }

    fun delete(item: WaddlePlanningItem) = operation {
        val room = _state.value.room ?: return@operation false
        if (!manager.eventPlanningRetract(room, item.id)) return@operation false
        _state.value = _state.value.copy(items = _state.value.items.filter { it.id != item.id })
        true
    }

    fun cancelPending() {
        operationJob?.cancel()
        operationJob = null
        _state.value = _state.value.copy(busy = false)
    }

    private fun operation(block: suspend () -> Boolean) {
        if (_state.value.busy) return
        _state.value = _state.value.copy(busy = true, failed = false)
        operationJob = viewModelScope.launch {
            try {
                val success = block()
                _state.value = _state.value.copy(busy = false, failed = !success)
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (_: Exception) { _state.value = _state.value.copy(busy = false, failed = true) }
        }
    }
}

internal fun newPlanningItem(kind: WaddlePlanningKind, title: String = ""): WaddlePlanningItem = WaddlePlanningItem(
    id = java.util.UUID.randomUUID().toString(), kind = kind, author = null, title = title,
    description = null, url = null, category = null, location = null, timezone = null,
    startsAt = null, endsAt = null, assignee = null, completed = false, pinned = false,
    amountMinor = null, currency = null, participants = emptyList(), options = emptyList(),
    selections = emptyList(), subject = null, attendance = null, status = null,
)
