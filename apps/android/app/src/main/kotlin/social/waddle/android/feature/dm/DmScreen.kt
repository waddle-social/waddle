package social.waddle.android.feature.dm

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.GroupAdd
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.IdleAge
import social.waddle.android.client.idleAgeOf
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.presenceShowsIdle
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.feature.conversation.ConversationKind
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.conversation.DmHeaderAvatar
import social.waddle.android.feature.conversation.DmHeaderSubline
import social.waddle.android.feature.conversation.SlashCommandHost
import social.waddle.android.feature.conversation.presenceWordOf
import social.waddle.android.feature.conversation.rememberExtensionCommandController
import social.waddle.android.feature.conversation.trustedPreviewOriginOf
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.jid.bareJidOf
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddlePresence

/** DM timeline + composer over the shared conversation scaffold. */
@Composable
fun DmScreen(
    peerJid: String,
    name: String,
    onBack: () -> Unit,
    onOpenThread: (threadId: String) -> Unit,
    onOpenGroupDm: (roomJid: String, name: String) -> Unit = { _, _ -> },
) {
    val graph = LocalAppGraph.current
    val viewModel: DmViewModel = viewModel(
        key = "dm:$peerJid",
        factory = DmViewModel.factory(graph, peerJid),
    )
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val contacts by graph.sessionManager.presenceStore.contacts.collectAsStateWithLifecycle()
    val directory by graph.sessionManager.peopleDirectory.collectAsStateWithLifecycle()
    val peerBare = normalizedBareJid(peerJid)
    // Same lookup as the DM list: the People directory (rooms and contacts,
    // most-available), then contact presence. Seen nowhere this stream is
    // unknown, not "offline": no dot and no presence word.
    val peer = contacts[peerBare]
    val peerShow: PresenceShow? = directory.people.firstOrNull { it.bareJid == peerBare }?.show ?: peer?.show
    val scope = rememberCoroutineScope()
    var newGroupOpen by remember { mutableStateOf(false) }

    // Capture permissions are requested BEFORE the XEP-0353 propose so
    // the responder's accept can go straight to media; a denial still
    // places the call receive-only (web best-effort capture parity).
    var pendingCallVideo by remember { mutableStateOf(false) }
    val callPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) {
        val video = pendingCallVideo
        scope.launch {
            graph.sessionManager.callStore.startCall(
                peerJid = peerJid,
                media = WaddleCallMedia(audio = true, video = video),
            )
        }
    }

    ConversationScreen(
        title = name,
        viewModel = viewModel,
        onBack = onBack,
        onOpenThread = onOpenThread,
        searchTarget = MessageSearchTarget(peerJid, isGroupchat = false),
        selfBareJid = session?.jid?.let(::bareJidOf),
        kind = ConversationKind.Dm(name),
        titleLeading = { DmHeaderAvatar(jid = bareJidOf(peerJid), name = name, presence = peerShow) },
        titleSubline = peerShow?.let { show -> { DmHeaderSubline(presenceWordOf(show, idleAge(peer?.presence))) } },
        trustedMediaOrigin = trustedPreviewOriginOf(session, graph.serverUrl),
        slashCommandHost = SlashCommandHost(
            controller = rememberExtensionCommandController(graph),
            roomJid = null,
        ),
        overflowItems = { dismiss ->
            // "Start a group from this DM": the create sheet opens with
            // the peer prefilled (web group-DM spawn parity).
            DropdownMenuItem(
                text = { Text(text = stringResource(R.string.dm_start_group_action)) },
                leadingIcon = { Icon(Icons.Outlined.GroupAdd, contentDescription = null) },
                onClick = {
                    dismiss()
                    newGroupOpen = true
                },
                modifier = Modifier.testTag(DmScreenTestTags.START_GROUP_ACTION),
            )
        },
        onStartCall = { video ->
            pendingCallVideo = video
            callPermissionLauncher.launch(
                if (video) {
                    arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
                } else {
                    arrayOf(Manifest.permission.RECORD_AUDIO)
                },
            )
        },
    )

    if (newGroupOpen) {
        NewGroupDmSheet(
            initialMembers = mapOf(bareJidOf(peerJid) to name),
            onDismiss = { newGroupOpen = false },
            onCreated = { roomJid, groupName ->
                newGroupOpen = false
                onOpenGroupDm(roomJid, groupName)
            },
        )
    }
}

/** Semantics tags shared with instrumented tests. */
object DmScreenTestTags {
    const val START_GROUP_ACTION = "dm-start-group-action"
}

/**
 * Web `ChatHeader.dmPeerPresenceText` parity: the XEP-0319 idle age while
 * the peer is away (`away`/`xa`) with an `idleSince` stamp, else `null`.
 * It re-derives on a minute ticker so it stays current without presence
 * traffic.
 */
@Composable
private fun idleAge(presence: WaddlePresence?): String? {
    if (presence == null || !presenceShowsIdle(presence.show)) return null
    val idleSince = presence.idleSince ?: return null
    var nowMs by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(idleSince) {
        while (true) {
            delay(MINUTE_MS)
            nowMs = System.currentTimeMillis()
        }
    }
    return idleAgeOf(idleSince, nowMs)?.let { idleAgeText(it) }
}

@Composable
private fun idleAgeText(age: IdleAge): String = when (age) {
    IdleAge.UnderMinute -> stringResource(R.string.idle_under_minute)
    is IdleAge.Minutes -> stringResource(R.string.idle_minutes, age.minutes)
    is IdleAge.Hours -> stringResource(R.string.idle_hours, age.hours)
    is IdleAge.Days -> stringResource(R.string.idle_days, age.days)
}

private const val MINUTE_MS = 60_000L
