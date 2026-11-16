package social.waddle.android.feature.events

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.client.EventPlanningFailure
import social.waddle.android.client.EventPlanningFailureReason
import social.waddle.android.client.EventPlanningOwner
import social.waddle.android.client.EventPlanningResult
import social.waddle.android.client.EventPlanningRetry
import social.waddle.android.client.EventPlanningStage
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
    val failure: EventPlanningFailure? = null,
)

internal class EventPlanningModel(
    private val manager: XmppSessionManager,
    private val selfJid: String,
) : ViewModel() {
    private val _state = MutableStateFlow(PlanningState())
    val state = _state.asStateFlow()
    private var operationJob: Job? = null
    private var owner = manager.captureEventPlanningOwner(selfJid)
    private var pendingCreate: EventPlanningRetry? = null
    private var operationGeneration = 0L
    init { refresh() }

    fun refresh() {
        cancelPending()
        val freshOwner = manager.captureEventPlanningOwner(selfJid)
        if (owner?.sameSessionAs(freshOwner) != true) {
            pendingCreate = null
            _state.value = PlanningState()
        }
        owner = freshOwner
        operation {
            val operationOwner = owner
            if (operationOwner == null) return@operation false
            val rooms = manager.fetchEventRooms() ?: return@operation false
            val room = _state.value.room
            val items = if (room != null) manager.eventPlanningItems(room) ?: return@operation false else emptyList()
            currentCoroutineContext().ensureActive()
            manager.applyPlanningIfCurrent(operationOwner) {
                _state.value = _state.value.copy(rooms = rooms, items = items)
            }
        }
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
        pendingCreate = null
        _state.value = _state.value.copy(room = null, items = emptyList())
        refresh()
    }

    /** A fresh editor must not inherit a failed or still-running initial refresh. */
    fun startEditor() {
        cancelPending()
        if (owner == null) owner = manager.captureEventPlanningOwner(selfJid)
        _state.value = _state.value.copy(failed = false, failure = null)
    }

    fun create(metadata: WaddlePlanningItem, onSaved: () -> Unit) = operation {
        val operationOwner = owner
        val generation = operationGeneration
        val result = manager.createPlanningEvent(operationOwner, metadata, pendingCreate) { created ->
            if (generation == operationGeneration) {
                pendingCreate = created
                _state.value = _state.value.copy(room = created.roomJid)
            }
        }
        currentCoroutineContext().ensureActive()
        applySaved(result, operationOwner, onSaved)
    }

    fun save(item: WaddlePlanningItem, onSaved: () -> Unit = {}) = operation {
        val room = _state.value.room ?: return@operation false
        val operationOwner = owner
        val result = manager.savePlanningItem(operationOwner, room, item)
        currentCoroutineContext().ensureActive()
        applySaved(result, operationOwner, onSaved)
    }

    private suspend fun applySaved(
        result: EventPlanningResult,
        operationOwner: EventPlanningOwner?,
        onSaved: () -> Unit,
    ): Boolean = when (result) {
        is EventPlanningResult.Failed -> {
            if (result.retry != null) pendingCreate = result.retry
            _state.value = _state.value.copy(failure = result.failure)
            false
        }
        is EventPlanningResult.Saved -> {
            val applied = manager.applyPlanningIfCurrent(operationOwner) {
                _state.value = _state.value.copy(
                    room = result.roomJid,
                    items = _state.value.items.filter { it.id != result.item.id } + result.item,
                    failed = false, failure = null,
                )
                pendingCreate = null
                onSaved()
            }
            if (!applied) {
                _state.value = _state.value.copy(
                    failure = EventPlanningFailure(
                        EventPlanningStage.SAVE_ITEM, EventPlanningFailureReason.SESSION_CHANGED,
                    ),
                )
            }
            applied
        }
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
        operationGeneration += 1
        operationJob?.cancel()
        operationJob = null
        _state.value = _state.value.copy(busy = false)
    }

    private fun operation(block: suspend () -> Boolean) {
        if (_state.value.busy) return
        val generation = ++operationGeneration
        _state.value = _state.value.copy(busy = true, failed = false, failure = null)
        operationJob = viewModelScope.launch {
            try {
                val success = block()
                if (generation == operationGeneration) {
                    _state.value = _state.value.copy(busy = false, failed = !success)
                }
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                if (generation == operationGeneration) _state.value = _state.value.copy(busy = false, failed = true)
            }
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
