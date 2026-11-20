package social.waddle.android.feature.events

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.huddle.HuddleCard
import social.waddle.client.ffi.WaddleCalendarDate
import social.waddle.client.ffi.WaddleEvent
import social.waddle.client.ffi.WaddleEventParticipation
import java.time.ZoneId

/** Calendar and event workspace, reachable from Home's calendar action. */
@Composable
fun CommunityCalendarScreen(onBack: () -> Unit, onOpenWorkspace: (String) -> Unit) {
    val graph = LocalAppGraph.current
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val selfJid = session?.jid ?: return
    val model: EventsViewModel = viewModel(key = "events:$selfJid", factory = EventsViewModel.factory(graph, selfJid))
    val state by model.state.collectAsStateWithLifecycle()
    DisposableEffect(model) {
        onDispose { model.cancelPending() }
    }
    var selectedId by rememberSaveable { mutableStateOf<String?>(null) }
    var editing by rememberSaveable { mutableStateOf(false) }
    var creating by rememberSaveable { mutableStateOf(false) }
    var confirmDelete by rememberSaveable { mutableStateOf(false) }
    val selected = state.events.firstOrNull { it.id == selectedId }
    val closeEditor = {
        model.cancelPending()
        editing = false
        creating = false
    }
    val back = {
        if (editing || creating) {
            closeEditor()
        } else if (selectedId != null) {
            selectedId = null
        } else {
            onBack()
        }
    }
    BackHandler(enabled = editing || creating || selectedId != null, onBack = back)
    Scaffold(
        topBar = {
            WaddleTopBar(
                title = { Text(selected?.summary ?: stringResource(R.string.events_title)) },
                navigationIcon = {
                    IconButton(onClick = back) {
                        Icon(Icons.AutoMirrored.Outlined.ArrowBack, stringResource(R.string.events_back))
                    }
                },
                actions = {
                    if (!editing && !creating && selected == null) {
                        IconButton(onClick = { creating = true }, enabled = !state.busy) {
                            Icon(Icons.Outlined.Add, stringResource(R.string.events_create))
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier.fillMaxSize().padding(padding)
                .verticalScroll(rememberScrollState()).padding(Waddle.space.md),
            verticalArrangement = Arrangement.spacedBy(Waddle.space.md),
        ) {
            if (state.busy) CircularProgressIndicator()
            if (state.failed) Text(stringResource(R.string.events_error), color = MaterialTheme.colorScheme.error)
            when {
                creating || editing -> EventEditor(
                    event = if (creating) null else selected,
                    busy = state.busy,
                    selfJid = selfJid,
                    onCancel = closeEditor,
                    onSave = { model.save(it, closeEditor) },
                )
                selected != null -> {
                    Text(selected.description.orEmpty())
                    Text(selected.location.orEmpty())
                    Text(eventDateText(selected.start).ifEmpty { stringResource(R.string.events_no_date) })
                    selected.end?.let { Text(eventDateText(it)) }
                    Text(stringResource(R.string.events_attendees), style = MaterialTheme.typography.titleMedium)
                    selected.attendees.forEach { Text("${it.uri.removePrefix("xmpp:")}: ${it.status}") }
                    listOf(
                        R.string.events_going to WaddleEventParticipation.ACCEPTED,
                        R.string.events_interested to WaddleEventParticipation.TENTATIVE,
                        R.string.events_declined to WaddleEventParticipation.DECLINED,
                    ).forEach { (label, status) ->
                        TextButton(onClick = { model.rsvp(selected, status) }, enabled = !state.busy) {
                            Text(stringResource(label))
                        }
                    }
                    val isOrganizer = selected.organizer?.removePrefix("xmpp:") == selfJid.substringBefore('/')
                    if (selected.workspaceRoom != null || isOrganizer) {
                        Button(onClick = { model.openPlanning(selected, onOpenWorkspace) }, enabled = !state.busy) {
                            Text(stringResource(R.string.events_workspaces))
                        }
                    }
                    if (isOrganizer) {
                        Button(onClick = { editing = true }, enabled = !state.busy) {
                            Text(stringResource(R.string.events_edit))
                        }
                        TextButton(onClick = { confirmDelete = true }, enabled = !state.busy) {
                            Text(stringResource(R.string.events_delete))
                        }
                    }
                }
                else -> {
                    if (state.events.isEmpty() && !state.busy) Text(stringResource(R.string.events_empty))
                    state.events.forEach { event ->
                        HuddleCard(onClick = { selectedId = event.id }, modifier = Modifier.fillMaxWidth()) {
                            Text(event.summary, style = MaterialTheme.typography.titleMedium)
                            Text(eventDateText(event.start).ifEmpty { stringResource(R.string.events_no_date) })
                            event.location?.let { Text(it) }
                        }
                    }
                    TextButton(onClick = { model.refresh() }, enabled = !state.busy) {
                        Text(stringResource(R.string.events_retry))
                    }
                }
            }
        }
    }
    if (confirmDelete && selected != null) {
        AlertDialog(
            onDismissRequest = { confirmDelete = false },
            text = { Text(stringResource(R.string.events_delete_confirm)) },
            confirmButton = {
                TextButton(onClick = {
                    confirmDelete = false
                    model.delete(selected) { selectedId = null }
                }) { Text(stringResource(R.string.events_delete)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmDelete = false }) { Text(stringResource(R.string.events_cancel)) }
            },
        )
    }
}

@Composable
internal fun EventEditor(
    event: WaddleEvent?,
    busy: Boolean,
    selfJid: String,
    onCancel: () -> Unit,
    onSave: (WaddleEvent) -> Unit,
) {
    var name by rememberSaveable(event?.id) { mutableStateOf(event?.summary.orEmpty()) }
    var description by rememberSaveable(event?.id) { mutableStateOf(event?.description.orEmpty()) }
    var location by rememberSaveable(event?.id) { mutableStateOf(event?.location.orEmpty()) }
    val initialTimezone = rememberSaveable(event?.id) { ZoneId.systemDefault().id }
    var timezone by rememberSaveable(event?.id) { mutableStateOf(initialTimezone) }
    var allDay by rememberSaveable(event?.id) {
        mutableStateOf(event?.start is WaddleCalendarDate.Date || event?.end is WaddleCalendarDate.Date)
    }
    var start by rememberSaveable(event?.id) { mutableStateOf(communityPickerText(event?.start, timezone, false)) }
    var end by rememberSaveable(event?.id) { mutableStateOf(communityPickerText(event?.end, timezone, true)) }
    val originalStart = (event?.start as? WaddleCalendarDate.DateTime)?.timestampMs
        .takeIf { timezone == initialTimezone }
    val originalEnd = (event?.end as? WaddleCalendarDate.DateTime)?.timestampMs
        .takeIf { timezone == initialTimezone }
    val dateIssue = eventDateIssue(start, end, timezone, allDay, originalStart, originalEnd)
    val valid = name.isNotBlank() && dateIssue == null
    EventField(name, { name = it }, R.string.events_name, !busy)
    EventField(description, { description = it }, R.string.events_description, !busy)
    EventField(location, { location = it }, R.string.events_location, !busy)
    Text(stringResource(R.string.events_dates_optional), style = MaterialTheme.typography.bodySmall)
    Row(
        modifier = Modifier.fillMaxWidth().toggleable(
            value = allDay, enabled = !busy, role = Role.Switch,
            onValueChange = { selected ->
                start = changeAllDay(start, selected, false)
                end = changeAllDay(end, selected, true)
                allDay = selected
            },
        ),
    ) {
        Text(stringResource(R.string.events_all_day), modifier = Modifier.weight(1f))
        Switch(checked = allDay, onCheckedChange = null, enabled = !busy)
    }
    if (!allDay) EventTimeZoneField(timezone, { timezone = it }, !busy)
    EventDateTimeField(start, { start = it }, stringResource(R.string.events_start), !busy, dateOnly = allDay)
    EventDateTimeField(
        end, { end = it }, stringResource(if (allDay) R.string.events_last_day else R.string.events_end),
        !busy, dateOnly = allDay,
    )
    dateIssue?.let { Text(stringResource(it.message), color = MaterialTheme.colorScheme.error) }
    if (!valid && name.isNotEmpty() && dateIssue == null) {
        Text(stringResource(R.string.events_invalid), color = MaterialTheme.colorScheme.error)
    }
    Row {
        Button(enabled = valid && !busy, onClick = {
            onSave(
                WaddleEvent(
                id = event?.id.orEmpty(), uid = event?.uid.orEmpty(), summary = name.trim(),
                description = description.trim().takeIf { it.isNotEmpty() },
                location = location.trim().takeIf { it.isNotEmpty() },
                organizer = event?.organizer ?: "xmpp:${selfJid.substringBefore('/')}",
                start = communityPickerValue(start, timezone, allDay, false, originalStart),
                end = communityPickerValue(end, timezone, allDay, true, originalEnd),
                attendees = event?.attendees.orEmpty(),
                workspaceRoom = event?.workspaceRoom,
                recurrence = event?.recurrence,
                excluded = event?.excluded.orEmpty(),
                overrides = event?.overrides.orEmpty(),
            )
            )
        }) { Text(stringResource(R.string.events_save)) }
        TextButton(onClick = onCancel, enabled = !busy) { Text(stringResource(R.string.events_cancel)) }
    }
}

@Composable
private fun EventField(value: String, onChange: (String) -> Unit, label: Int, enabled: Boolean) {
    OutlinedTextField(
        value, onChange,
        label = { Text(stringResource(label)) },
        modifier = Modifier.fillMaxWidth(),
        enabled = enabled,
    )
}
