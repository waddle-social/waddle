package social.waddle.android.feature.channel

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.launch
import social.waddle.android.AppGraph
import social.waddle.android.DEFAULT_NICK
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.feature.call.ChannelCallBanner
import social.waddle.android.feature.call.ChannelCallControls
import social.waddle.android.feature.call.ChannelCallTestTags
import social.waddle.android.feature.call.RetainedSessionView
import social.waddle.android.feature.call.RoomMujiView
import social.waddle.android.feature.call.SelfCallIdentity
import social.waddle.android.feature.call.channelCallBannerOf
import social.waddle.android.feature.call.channelCallControlsOf
import social.waddle.android.feature.call.normalizeCallRoomJid
import social.waddle.android.feature.call.resolveRoomParticipantList
import social.waddle.android.feature.call.shouldOfferRetainedLeave
import social.waddle.android.feature.conversation.ConversationScreen
import social.waddle.android.feature.conversation.trustedPreviewOriginOf
import social.waddle.android.feature.room.RoomSettingsSheet
import social.waddle.android.feature.room.RoomSettingsViewModel
import social.waddle.android.feature.search.MessageSearchTarget
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.domainOf
import social.waddle.client.ffi.WaddleCallMedia

/** Channel timeline + composer over the shared conversation scaffold. */
@Composable
fun ChannelScreen(
    roomJid: String,
    name: String,
    onBack: () -> Unit,
    onOpenThread: (threadId: String) -> Unit,
    onOpenMembers: () -> Unit = {},
) {
    val graph = LocalAppGraph.current
    val viewModel: ChannelViewModel = viewModel(
        key = "channel:$roomJid",
        factory = ChannelViewModel.factory(graph, roomJid),
    )
    val session by graph.currentSession.collectAsStateWithLifecycle()
    val isRoomOwner by viewModel.isRoomOwner.collectAsStateWithLifecycle()
    var roomSettingsOpen by remember { mutableStateOf(false) }

    val callState by graph.sessionManager.callStore.state.collectAsStateWithLifecycle()
    val mucPresence = graph.sessionManager.callStore.mucCallPresence
    val participants by mucPresence.participants.collectAsStateWithLifecycle()
    val owners by mucPresence.owners.collectAsStateWithLifecycle()
    val selfLeaveEchoPending by mucPresence.selfLeaveEchoPending.collectAsStateWithLifecycle()
    val roomMedia by mucPresence.media.collectAsStateWithLifecycle()
    val liveParticipants by graph.mucCallLiveParticipants.participants.collectAsStateWithLifecycle()
    val leavingRooms by graph.mucCallLiveParticipants.leavingRooms.collectAsStateWithLifecycle()

    val normalizedRoom = remember(roomJid) { normalizeCallRoomJid(roomJid) }
    val resolvedNicks = resolveRoomParticipantList(
        normalizedRoom, participants, owners, liveParticipants, leavingRooms,
    )
    val roomHasVideoCall = roomMedia[normalizedRoom]?.video == true

    // Retained-call cleanup (web leaveRetainedMucCallAction wiring):
    // re-read the cache whenever the Muji view or the slot moves, plus
    // after an explicit leave tap (the bump), so the affordance clears.
    var retainedRefresh by remember { mutableIntStateOf(0) }
    val retainedSession by produceState<RetainedSessionView?>(
        initialValue = null, normalizedRoom, participants, callState, retainedRefresh,
    ) {
        value = graph.sessionPrefs.mucCallSessions
            .retainedEntry(normalizedRoom, graph.sessionManager.ownFullJid())
            ?.let { RetainedSessionView(terminatePending = it.terminatePending, selfFullJid = it.selfFullJid) }
    }
    val retainedLeave = shouldOfferRetainedLeave(
        state = callState,
        roomJid = roomJid,
        retained = retainedSession,
        muji = RoomMujiView(
            nicks = participants[normalizedRoom]?.toList() ?: emptyList(),
            owners = owners[normalizedRoom] ?: emptyMap(),
        ),
        self = SelfCallIdentity(
            nick = session?.xmppLocalpart,
            fullJid = graph.sessionManager.ownFullJid(),
            leaveEchoPending = session?.xmppLocalpart
                ?.let { "$normalizedRoom/$it" in selfLeaveEchoPending } == true,
        ),
    )

    fun leaveRetainedCall() {
        val nick = session?.xmppLocalpart ?: DEFAULT_NICK
        val selfFullJid = graph.sessionManager.ownFullJid()
        graph.applicationScope.launch {
            graph.sessionManager.callStore.muc.leaveRetained(roomJid, nick, selfFullJid)
            retainedRefresh += 1
        }
    }

    // Capture permissions are requested BEFORE the XEP-0272 setup so
    // the mixer's accept can go straight to media; a denial still
    // places the call receive-only (DM-button parity).
    var pendingCallVideo by remember { mutableStateOf(false) }
    val callPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) {
        startOrJoinMucCall(graph, roomJid, video = pendingCallVideo)
    }

    fun requestCall(video: Boolean) {
        pendingCallVideo = video
        callPermissionLauncher.launch(
            if (video) {
                arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
            } else {
                arrayOf(Manifest.permission.RECORD_AUDIO)
            },
        )
    }

    ConversationScreen(
        title = name,
        viewModel = viewModel,
        onBack = onBack,
        onOpenThread = onOpenThread,
        searchTarget = MessageSearchTarget(roomJid, isGroupchat = true),
        selfBareJid = session?.jid?.let(::bareJidOf),
        trustedMediaOrigin = trustedPreviewOriginOf(session, graph.serverUrl),
        extraTopBarActions = {
            ChannelCallTopBarActions(
                controls = channelCallControlsOf(callState, roomJid, resolvedNicks, roomHasVideoCall),
                onStart = ::requestCall,
            )
            IconButton(
                onClick = onOpenMembers,
                modifier = Modifier.testTag(ChannelScreenTestTags.MEMBERS_ACTION),
            ) {
                Icon(
                    Icons.Outlined.Group,
                    contentDescription = stringResource(R.string.members_action),
                )
            }
            // XEP-0045 §10 owner use cases: settings entry is
            // owner-gated by the self-presence affiliation; the
            // server enforces ownership on every config IQ anyway.
            if (isRoomOwner) {
                IconButton(
                    onClick = { roomSettingsOpen = true },
                    modifier = Modifier.testTag(ChannelScreenTestTags.ROOM_SETTINGS_ACTION),
                ) {
                    Icon(
                        Icons.Outlined.Settings,
                        contentDescription = stringResource(R.string.room_settings_title),
                    )
                }
            }
        },
        aboveTimeline = {
            ChannelCallBanner(
                banner = channelCallBannerOf(
                    callState, roomJid, resolvedNicks, roomHasVideoCall,
                    retainedLeave = retainedLeave,
                ),
                onJoin = { requestCall(video = roomHasVideoCall) },
                onLeaveRetained = ::leaveRetainedCall,
            )
        },
    )

    if (roomSettingsOpen) {
        val settingsViewModel: RoomSettingsViewModel = viewModel(
            key = "room-settings:$roomJid",
            factory = RoomSettingsViewModel.factory(graph, roomJid),
        )
        RoomSettingsSheet(
            viewModel = settingsViewModel,
            onDismiss = { roomSettingsOpen = false },
            onDestroyed = {
                roomSettingsOpen = false
                onBack()
            },
        )
    }
}

/**
 * Top-bar call slot for channels: start-audio/start-video while the
 * room is call-free, a single join affordance while a call is live,
 * nothing while our slot is busy (web MucCallButton semantics).
 */
@Composable
private fun ChannelCallTopBarActions(
    controls: ChannelCallControls,
    onStart: (video: Boolean) -> Unit,
) {
    when (controls) {
        ChannelCallControls.Hidden -> Unit
        ChannelCallControls.Start -> {
            IconButton(
                onClick = { onStart(false) },
                modifier = Modifier.testTag(ChannelCallTestTags.TOP_BAR_AUDIO),
            ) {
                Icon(
                    Icons.Outlined.Call,
                    contentDescription = stringResource(R.string.call_start_audio),
                )
            }
            IconButton(
                onClick = { onStart(true) },
                modifier = Modifier.testTag(ChannelCallTestTags.TOP_BAR_VIDEO),
            ) {
                Icon(
                    Icons.Outlined.Videocam,
                    contentDescription = stringResource(R.string.call_start_video),
                )
            }
        }
        is ChannelCallControls.Join -> IconButton(
            onClick = { onStart(controls.video) },
            modifier = Modifier.testTag(ChannelCallTestTags.TOP_BAR_JOIN),
        ) {
            Icon(
                if (controls.video) Icons.Outlined.Videocam else Icons.Outlined.Call,
                contentDescription = stringResource(R.string.call_action_join),
            )
        }
    }
}

/**
 * Start or join the room's group call on the application scope — the
 * XEP-0272 setup runs multi-second waits that must survive this
 * screen's disposal. Resume-first (web `tryResumeFirst`): a cached
 * post-process-death session promotes straight to Active without a
 * fresh Jingle attempt; otherwise the full §Joining flow runs.
 */
private fun startOrJoinMucCall(graph: AppGraph, roomJid: String, video: Boolean) {
    val nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK
    val accountJid = graph.currentSession.value?.jid
    val selfFullJid = graph.sessionManager.ownFullJid()
    val store = graph.sessionManager.callStore
    graph.applicationScope.launch {
        if (store.muc.resume(roomJid, nick, selfFullJid)) return@launch
        store.muc.begin(
            roomJid = roomJid,
            media = WaddleCallMedia(audio = true, video = video),
            selfNick = nick,
            selfFullJid = selfFullJid,
            expectedMixerJid = accountJid
                ?.let(::domainOf)
                ?.takeIf { it.isNotEmpty() }
                ?.let { domain -> "calls.$domain" },
        )
    }
}

/** Semantics tags shared with instrumented tests. */
object ChannelScreenTestTags {
    const val MEMBERS_ACTION = "channel-members-action"
    const val ROOM_SETTINGS_ACTION = "channel-room-settings-action"
}
