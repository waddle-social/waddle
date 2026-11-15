package social.waddle.android.feature.events

import android.content.Intent
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.huddle.HuddleCard
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleAttendanceStatus
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddlePlanningItem
import social.waddle.client.ffi.WaddlePlanningKind

@Composable
fun EventsScreen(
    onBack: () -> Unit,
    onOpenConversation: (String, String) -> Unit,
    onOpenMembers: (String, String) -> Unit,
) {
    val graph = LocalAppGraph.current
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val self = session?.jid?.substringBefore('/') ?: return
    val model: EventPlanningModel = viewModel(
        key = "planning:$self", factory = viewModelFactoryOf { EventPlanningModel(graph.sessionManager, self) },
    )
    val state by model.state.collectAsStateWithLifecycle()
    val memberRooms by graph.sessionManager.roomMembersStore.rooms.collectAsStateWithLifecycle()
    LaunchedEffect(state.room) {
        state.room?.let { graph.sessionManager.refreshRoomMembers(it) }
    }
    val mayManage = memberRooms[state.room]?.members?.any {
        it.jid.substringBefore('/') == self &&
            it.affiliation in listOf(WaddleMucAffiliation.OWNER, WaddleMucAffiliation.ADMIN)
    } == true
    var calendar by rememberSaveable { mutableStateOf(false) }
    var editingId by rememberSaveable { mutableStateOf<String?>(null) }
    var addingKind by rememberSaveable { mutableStateOf<String?>(null) }
    var creating by rememberSaveable { mutableStateOf(false) }
    var roomAddress by rememberSaveable { mutableStateOf("") }
    var filter by rememberSaveable { mutableStateOf<String?>(null) }
    val closeEditor = {
        editingId = null;
    addingKind = null;
    creating = false
    }
    val draftId = rememberSaveable(creating, addingKind) { java.util.UUID.randomUUID().toString() }
    val editing = state.items.firstOrNull { it.id == editingId }
    val metadata = state.items.firstOrNull { it.kind == WaddlePlanningKind.EVENT }
    val context = LocalContext.current
    val back = {
        if (creating || editingId != null || addingKind != null) {
            closeEditor()
        } else if (state.room != null) {
            model.close()
        } else {
            onBack()
        }
    }
    BackHandler(enabled = creating || editingId != null || addingKind != null || state.room != null, onBack = back)
    if (calendar) {
        CommunityCalendarScreen(
            onBack = { calendar = false },
            onOpenWorkspace = { room ->
                calendar = false;
            model.open(room)
            },
        )
        return
    }
    Scaffold(topBar = {
        WaddleTopBar(
            title = { Text(metadata?.title ?: stringResource(R.string.events_workspaces)) },
            navigationIcon = {
                IconButton(onClick = back) {
                    Icon(Icons.AutoMirrored.Outlined.ArrowBack, stringResource(R.string.events_back))
                }
            },
        )
    }) { padding ->
        Column(
            modifier = Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState())
                .padding(Waddle.space.md),
            verticalArrangement = Arrangement.spacedBy(Waddle.space.md),
        ) {
            if (state.busy) CircularProgressIndicator()
            if (state.failed) Text(stringResource(R.string.events_error), color = MaterialTheme.colorScheme.error)
            when {
                creating || editing != null || addingKind != null -> PlanningEditor(
                    initial = editing ?: newPlanningItem(
                        if (creating) WaddlePlanningKind.EVENT else WaddlePlanningKind.valueOf(addingKind!!),
                    ).copy(id = draftId),
                    busy = state.busy,
                    onCancel = closeEditor,
                    onSave = { if (creating) model.create(it, closeEditor) else model.save(it, closeEditor) },
                )
                state.room == null -> {
                    Text(stringResource(R.string.events_private_notice))
                    Button(onClick = { creating = true }, enabled = !state.busy) {
                        Text(stringResource(R.string.events_create))
                    }
                    OutlinedTextField(
                        roomAddress, { roomAddress = it },
                        label = { Text(stringResource(R.string.events_room_address)) },
                        modifier = Modifier.fillMaxWidth(),
                    )
                    TextButton(
                        onClick = { model.open(roomAddress.trim()) },
                        enabled = !state.busy && roomAddress.trim().startsWith("event-") && '@' in roomAddress,
                    ) { Text(stringResource(R.string.events_open_room)) }
                    state.rooms.forEach { room ->
                        HuddleCard(onClick = { model.open(room.jid) }, modifier = Modifier.fillMaxWidth()) {
                            Text(room.name ?: room.jid)
                        }
                    }
                    TextButton(onClick = { calendar = true }) { Text(stringResource(R.string.events_public_calendar)) }
                    TextButton(onClick = { model.refresh() }, enabled = !state.busy) {
                        Text(stringResource(R.string.events_retry))
                    }
                }
                else -> {
                    val room = state.room!!
                    if (metadata == null) {
                        Button(onClick = { addingKind = WaddlePlanningKind.EVENT.name }, enabled = !state.busy) {
                            Text(stringResource(R.string.events_create))
                        }
                    }
                    Button(onClick = { onOpenConversation(room, metadata?.title ?: room) }) {
                        Text(stringResource(R.string.events_conversation))
                    }
                    Text(stringResource(R.string.events_private_notice))
                    TextButton(onClick = { onOpenMembers(room, metadata?.title ?: room) }) {
                        Text(stringResource(R.string.events_members))
                    }
                    FlowRow {
                        listOf(
                            R.string.events_going to WaddleAttendanceStatus.GOING,
                            R.string.events_interested to WaddleAttendanceStatus.INTERESTED,
                            R.string.events_declined to WaddleAttendanceStatus.DECLINED,
                        ).forEach { (label, status) ->
                            TextButton(enabled = !state.busy, onClick = {
                                model.save(
                                    newPlanningItem(WaddlePlanningKind.ATTENDANCE, self)
                                    .copy(attendance = status)
                                )
                            }) { Text(stringResource(label)) }
                        }
                    }
                    TextButton(onClick = {
                        val directory = java.io.File(context.cacheDir, "event_exports").apply { mkdirs() }
                        val target = java.io.File(directory, "event.ics")
                        target.writeText(planningCalendar(state.items, room))
                        val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", target)
                        val intent = Intent(Intent.ACTION_SEND).apply {
                            type = "text/calendar"
                            putExtra(Intent.EXTRA_STREAM, uri)
                            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                        }
                        context.startActivity(Intent.createChooser(intent, null))
                    }) { Text(stringResource(R.string.events_export)) }
                    FlowRow {
                        planningKinds.forEach { (kind, label) ->
                            TextButton(onClick = { addingKind = kind.name }, enabled = !state.busy) {
                                Text("+ ${stringResource(label)}")
                            }
                        }
                    }
                    FlowRow {
                        TextButton(onClick = { filter = null }) { Text(stringResource(R.string.events_all)) }
                        planningKinds.forEach { (kind, label) ->
                            TextButton(onClick = { filter = kind.name }) { Text(stringResource(label)) }
                        }
                    }
                    state.items.filter { filter == null || it.kind.name == filter }
                        .sortedWith(compareByDescending<WaddlePlanningItem> { it.pinned }.thenBy { it.startsAt })
                        .forEach { item ->
                            PlanningCard(
                                item = item, items = state.items, self = self, busy = state.busy,
                                mayEdit = mayManage || item.author == self ||
                                    item.kind == WaddlePlanningKind.TASK && item.assignee == self,
                                mayDelete = mayManage || item.author == self,
                                onEdit = { editingId = item.id }, onSave = { model.save(it) },
                                onDelete = { model.delete(item) },
                                onDiscuss = { onOpenConversation(room, metadata?.title ?: room) },
                            )
                        }
                    TextButton(onClick = { model.refresh() }, enabled = !state.busy) {
                        Text(stringResource(R.string.events_retry))
                    }
                }
            }
        }
    }
}

internal val planningKinds = listOf(
    WaddlePlanningKind.LINK to R.string.events_links,
    WaddlePlanningKind.TASK to R.string.events_tasks,
    WaddlePlanningKind.ITINERARY to R.string.events_itinerary,
    WaddlePlanningKind.DECISION to R.string.events_decisions,
    WaddlePlanningKind.TRAVEL to R.string.events_travel,
    WaddlePlanningKind.EXPENSE to R.string.events_expenses,
    WaddlePlanningKind.POLL to R.string.events_polls,
)
