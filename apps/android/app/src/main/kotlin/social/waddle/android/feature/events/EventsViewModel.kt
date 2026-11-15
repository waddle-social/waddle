package social.waddle.android.feature.events

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleCalendarDate
import social.waddle.client.ffi.WaddleEvent
import social.waddle.client.ffi.WaddleEventParticipation
import java.time.LocalDate

data class EventsState(
    val events: List<WaddleEvent> = emptyList(),
    val busy: Boolean = false,
    val failed: Boolean = false,
)

/** Remote acknowledgements drive the calendar; failed writes leave the editor open. */
class EventsViewModel(
    private val manager: XmppSessionManager,
    private val selfJid: String,
) : ViewModel() {
    private val communityJid = "community.${selfJid.substringBefore('/').substringAfter('@')}"
    private val _state = MutableStateFlow(EventsState())
    private val pendingWorkspaces = mutableMapOf<String, String>()
    val state = _state.asStateFlow()

    init { refresh() }

    fun refresh() = operation {
        val events = manager.fetchCommunityEvents(communityJid) ?: return@operation false
        _state.value = _state.value.copy(events = events.filter { it.summary.isNotBlank() })
        true
    }

    fun save(event: WaddleEvent, onSaved: () -> Unit) = operation {
        val saved = manager.publishCommunityEvent(communityJid, event) ?: return@operation false
        _state.value = _state.value.copy(events = listOf(saved) + _state.value.events.filter { it.id != saved.id })
        onSaved()
        true
    }

    fun delete(event: WaddleEvent, onDeleted: () -> Unit) = operation {
        if (!manager.retractCommunityEvent(communityJid, event.id)) return@operation false
        _state.value = _state.value.copy(events = _state.value.events.filter { it.id != event.id })
        onDeleted()
        true
    }

    fun rsvp(event: WaddleEvent, status: WaddleEventParticipation) = operation {
        val accepted = manager.rsvpCommunityEvent(communityJid, event.uid, selfJid.substringBefore('/'), status)
        if (!accepted) return@operation false
        val events = manager.fetchCommunityEvents(communityJid) ?: return@operation false
        _state.value = _state.value.copy(events = events.filter { it.summary.isNotBlank() })
        true
    }

    fun openPlanning(event: WaddleEvent, onOpen: (String) -> Unit) = operation {
        event.workspaceRoom?.let {
            onOpen(it);
        return@operation true
        }
        val room = pendingWorkspaces[event.id] ?: manager.createEventRoom(event.summary, true)
            ?: return@operation false
        pendingWorkspaces[event.id] = room
        val metadata = newPlanningItem(social.waddle.client.ffi.WaddlePlanningKind.EVENT, event.summary).copy(
            id = "event", description = event.description, location = event.location,
            timezone = "UTC", startsAt = calendarInstant(event.start), endsAt = calendarInstant(event.end),
        )
        if (manager.eventPlanningPublish(room, metadata) == null) return@operation false
        val saved = manager.publishCommunityEvent(communityJid, event.copy(workspaceRoom = room))
            ?: return@operation false
        _state.value = _state.value.copy(events = _state.value.events.map { if (it.id == saved.id) saved else it })
        pendingWorkspaces.remove(event.id)
        onOpen(room)
        true
    }

    private fun operation(block: suspend () -> Boolean) {
        if (_state.value.busy) return
        _state.value = _state.value.copy(busy = true, failed = false)
        viewModelScope.launch {
            try {
                val success = block()
                _state.value = _state.value.copy(busy = false, failed = !success)
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                _state.value = _state.value.copy(busy = false, failed = true)
            }
        }
    }

    companion object {
        fun factory(graph: AppGraph, selfJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf { EventsViewModel(graph.sessionManager, selfJid) }
    }
}

/** Keep calendar dates as dates and timed entries as explicit instants. */
internal fun parseEventDate(value: String): WaddleCalendarDate? {
    val text = value.trim()
    if (text.isEmpty()) return null
    return if (text.length == 10) {
        WaddleCalendarDate.Date(LocalDate.parse(text).toString())
    } else {
        WaddleCalendarDate.DateTime(java.time.OffsetDateTime.parse(text).toInstant().toEpochMilli())
    }
}

internal fun eventDatesValid(start: String, end: String): Boolean = try {
    val from = parseEventDate(start)
    val until = parseEventDate(end)
    when {
        until == null -> true
        from == null -> false
        from is WaddleCalendarDate.Date && until is WaddleCalendarDate.Date -> until.date > from.date
        from is WaddleCalendarDate.DateTime && until is WaddleCalendarDate.DateTime ->
            until.timestampMs > from.timestampMs
        else -> false
    }
} catch (_: java.time.DateTimeException) { false }

internal fun eventDateText(value: WaddleCalendarDate?): String = when (value) {
    is WaddleCalendarDate.Date -> value.date
    is WaddleCalendarDate.DateTime -> java.time.Instant.ofEpochMilli(value.timestampMs).toString()
    null -> ""
}

private fun calendarInstant(value: WaddleCalendarDate?): Long? = when (value) {
    is WaddleCalendarDate.Date -> LocalDate.parse(value.date).atStartOfDay(java.time.ZoneOffset.UTC)
        .toInstant().toEpochMilli()
    is WaddleCalendarDate.DateTime -> value.timestampMs
    null -> null
}
