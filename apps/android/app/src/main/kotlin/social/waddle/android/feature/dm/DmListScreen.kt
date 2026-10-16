package social.waddle.android.feature.dm

import android.text.format.DateFormat
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.GroupAdd
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.ConversationListRow
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.KickerText
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.LiveDot
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone
import social.waddle.android.ui.listTimeOf
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId

/**
 * The merged DM surface: 1:1 peers and group DMs by inbox recency, with
 * who is here on every avatar and ember for anything live. As a tab it
 * has no back affordance ([onBack] null).
 */
@Composable
fun DmListScreen(
    onOpenDm: (peerJid: String, name: String) -> Unit,
    onOpenGroupDm: (roomJid: String, name: String) -> Unit,
    onBack: (() -> Unit)? = null,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
) {
    val graph = LocalAppGraph.current
    val viewModel: DmListViewModel = viewModel(factory = DmListViewModel.factory(graph))
    val items by viewModel.items.collectAsStateWithLifecycle()
    var newGroupOpen by remember { mutableStateOf(false) }
    val context = LocalContext.current

    DmListContent(
        items = items,
        onOpenDm = onOpenDm,
        onOpenGroupDm = onOpenGroupDm,
        onNewGroup = { newGroupOpen = true },
        onBack = onBack,
        contentWindowInsets = contentWindowInsets,
        clock = ListClock(use24Hour = DateFormat.is24HourFormat(context)),
    )

    if (newGroupOpen) {
        NewGroupDmSheet(
            onDismiss = { newGroupOpen = false },
            onCreated = { roomJid, name ->
                newGroupOpen = false
                onOpenGroupDm(roomJid, name)
            },
        )
    }
}

/** How list times are read: injected so screenshots pin the day and clock. */
internal data class ListClock(
    val zone: ZoneId = ZoneId.systemDefault(),
    val today: LocalDate = LocalDate.now(zone),
    val use24Hour: Boolean? = null,
)

/** Stateless DM list: rows, or an invitation when there are none. */
@Composable
internal fun DmListContent(
    items: List<DmListItem>,
    onOpenDm: (peerJid: String, name: String) -> Unit,
    onOpenGroupDm: (roomJid: String, name: String) -> Unit,
    onNewGroup: () -> Unit,
    onBack: (() -> Unit)? = null,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
    clock: ListClock = ListClock(),
) {
    Scaffold(
        contentWindowInsets = contentWindowInsets,
        containerColor = Waddle.colors.bg,
        topBar = {
            WaddleTopBar(
                title = { Text(text = stringResource(R.string.dm_list_title)) },
                navigationIcon = {
                    if (onBack != null) {
                        IconButton(onClick = onBack) {
                            Icon(
                                Icons.AutoMirrored.Outlined.ArrowBack,
                                contentDescription = stringResource(R.string.action_back),
                            )
                        }
                    }
                },
                actions = {
                    IconButton(
                        onClick = onNewGroup,
                        modifier = Modifier.testTag(DmListTestTags.NEW_GROUP_ACTION),
                    ) {
                        Icon(
                            Icons.Outlined.GroupAdd,
                            contentDescription = stringResource(R.string.dm_list_new_group_action),
                        )
                    }
                },
            )
        },
    ) { padding ->
        if (items.isEmpty()) {
            EmptyState(
                icon = Icons.Outlined.ChatBubbleOutline,
                title = stringResource(R.string.dm_list_empty_title),
                body = stringResource(R.string.huddle_dm_list_empty_body),
                action = {
                    PillButton(
                        text = stringResource(R.string.huddle_dm_list_start_group),
                        onClick = onNewGroup,
                        tone = PillTone.QUIET,
                        icon = Icons.Outlined.GroupAdd,
                    )
                },
                modifier = Modifier
                    .padding(padding)
                    .consumeWindowInsets(padding),
            )
        } else {
            LazyColumn(
                contentPadding = PaddingValues(
                    top = padding.calculateTopPadding() + Waddle.space.xxs,
                    bottom = padding.calculateBottomPadding() + Waddle.space.md,
                ),
                modifier = Modifier.consumeWindowInsets(padding),
            ) {
                items(items = items, key = { dmRowKey(it.row) }, contentType = { "dm" }) { item ->
                    DmRow(item = item, clock = clock, onOpenDm = onOpenDm, onOpenGroupDm = onOpenGroupDm)
                }
            }
        }
    }
}

@Composable
private fun DmRow(
    item: DmListItem,
    clock: ListClock,
    onOpenDm: (peerJid: String, name: String) -> Unit,
    onOpenGroupDm: (roomJid: String, name: String) -> Unit,
) {
    val time = item.row.lastUpdated?.let {
        listTimeOf(Instant.ofEpochMilli(it), zone = clock.zone, today = clock.today, use24Hour = clock.use24Hour)
    }
    when (val row = item.row) {
        is DmListRow.Peer -> ConversationListRow(
            title = row.name,
            leading = {
                HuddleAvatar(
                    jid = row.peerJid,
                    name = row.name,
                    size = AvatarSize.LG,
                    presence = item.presence,
                    inHuddle = item.inHuddle,
                )
            },
            onClick = { onOpenDm(row.peerJid, row.name) },
            preview = row.preview ?: presenceLine(item.presence, item.inHuddle),
            time = time,
            unreadCount = row.unreadCount,
            mentionCount = item.mentionCount,
            isMuted = row.isMuted,
            conversationJid = row.peerJid,
            trailingExtra = { LiveMark(item.live) },
            modifier = Modifier.testTag(DmListTestTags.PEER_ROW_PREFIX + row.peerJid),
        )
        is DmListRow.Group -> ConversationListRow(
            title = row.name,
            leading = { GlyphTile(Icons.Outlined.Group, size = AvatarSize.LG.dp) },
            onClick = { onOpenGroupDm(row.roomJid, row.name) },
            preview = row.preview ?: stringResource(R.string.dm_list_group_subtitle),
            time = time,
            unreadCount = row.unreadCount,
            mentionCount = item.mentionCount,
            isMuted = row.isMuted,
            conversationJid = row.roomJid,
            trailingExtra = { LiveMark(item.live) },
            modifier = Modifier.testTag(DmListTestTags.GROUP_ROW_PREFIX + row.roomJid),
        )
    }
}

/** Ember dot plus "Live" / "Ringing": the only warm thing on a quiet row. */
@Composable
private fun LiveMark(live: DmLiveState) {
    val label = when (live) {
        DmLiveState.NONE -> return
        DmLiveState.RINGING -> stringResource(R.string.huddle_dm_ringing)
        DmLiveState.LIVE -> stringResource(R.string.huddle_dm_live)
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
    ) {
        LiveDot()
        KickerText(text = label, color = Waddle.colors.liveText)
    }
}

/** A peer with no preview yet: where they are, or an invitation. */
@Composable
private fun presenceLine(presence: PresenceShow?, inHuddle: Boolean): String = when {
    inHuddle -> stringResource(R.string.huddle_status_in_huddle)
    presence == PresenceShow.AVAILABLE -> stringResource(R.string.huddle_status_around)
    presence == PresenceShow.AWAY || presence == PresenceShow.XA -> stringResource(R.string.huddle_status_away)
    presence == PresenceShow.DND -> stringResource(R.string.huddle_status_dnd)
    else -> stringResource(R.string.huddle_dm_say_hello)
}

private fun dmRowKey(row: DmListRow): String = when (row) {
    is DmListRow.Peer -> "peer:${row.peerJid}"
    is DmListRow.Group -> "group:${row.roomJid}"
}

/** Semantics tags shared with instrumented tests. */
object DmListTestTags {
    const val NEW_GROUP_ACTION = "dm-list-new-group-action"
    const val PEER_ROW_PREFIX = "dm-list-peer:"
    const val GROUP_ROW_PREFIX = "dm-list-group:"
}
