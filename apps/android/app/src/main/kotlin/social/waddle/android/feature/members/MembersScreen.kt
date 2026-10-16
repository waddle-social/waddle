package social.waddle.android.feature.members

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.PersonAdd
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.RoomAdminResult
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.store.MemberListStatus
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.jid.bareJidOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.ChoiceOption
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.GroupedRow
import social.waddle.android.ui.HuddleFieldShape
import social.waddle.android.ui.KickerText
import social.waddle.android.ui.SectionKicker
import social.waddle.android.ui.SegmentedChoice
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar
import social.waddle.android.ui.huddle.HuddleCard
import social.waddle.android.ui.huddle.LiveDot
import social.waddle.android.ui.huddle.PillButton
import social.waddle.android.ui.huddle.PillTone
import social.waddle.android.ui.huddle.SkeletonBlock
import social.waddle.android.ui.huddle.glow
import social.waddle.android.ui.huddleFieldColors
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddleUserSearchEntry

/** A pending destructive member action awaiting confirmation. */
private sealed interface PendingAction {
    val row: MemberRow

    data class Kick(override val row: MemberRow) : PendingAction

    data class Ban(override val row: MemberRow) : PendingAction

    data class Remove(override val row: MemberRow) : PendingAction
}

/** Which members the list shows (web Members header segmented filter). */
enum class MemberFilter {
    /** Present in the room now. */
    HERE_NOW,
    EVERYONE,
}

/** What the members list can ask for. */
internal data class MembersListActions(
    val onRowClick: (MemberRow) -> Unit = {},
    val onFilter: (MemberFilter) -> Unit = {},
    val onSearchQueryChanged: (String) -> Unit = {},
    val onAddMember: (WaddleUserSearchEntry) -> Unit = {},
)

/**
 * Member screen (web `MemberManagement.vue` parity, Huddle cards):
 * who is in the room by presence (the huddle first, then here now, then
 * everyone else), each a card with avatar, presence and role; tapping
 * one opens their profile sheet, which carries the owner/admin actions
 * (promote, demote, kick, ban, remove) where the account may manage.
 * Owner rows are immutable. Declared room bots list last in their own
 * section: no presence, no actions. [onOpenDm] adds "Message" to the
 * profile sheet.
 */
@Composable
fun MembersScreen(
    roomJid: String,
    name: String,
    onBack: () -> Unit,
    onOpenDm: ((peerJid: String, name: String) -> Unit)? = null,
) {
    val graph = LocalAppGraph.current
    val viewModel: MembersViewModel = viewModel(
        key = "members:$roomJid",
        factory = MembersViewModel.factory(graph, roomJid),
    )
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val session by graph.currentSession.collectAsStateWithLifecycle()
    var filter by rememberSaveable { mutableStateOf(MemberFilter.HERE_NOW) }
    var profileTarget by remember { mutableStateOf<MemberRow?>(null) }
    var pendingAction by remember { mutableStateOf<PendingAction?>(null) }
    val snackbarHostState = remember { SnackbarHostState() }
    val failedText = stringResource(R.string.action_failed)
    val failedOfflineText = stringResource(R.string.action_failed_offline)
    val notPermittedText = stringResource(R.string.members_action_not_permitted)

    LaunchedEffect(viewModel) {
        viewModel.actionFailures.collect { failure ->
            snackbarHostState.showSnackbar(
                when (failure) {
                    RoomAdminResult.NotConnected -> failedOfflineText
                    RoomAdminResult.NotPermitted -> notPermittedText
                    else -> failedText
                },
            )
        }
    }

    Scaffold(
        containerColor = Waddle.colors.bg,
        snackbarHost = { SnackbarHost(snackbarHostState) },
        topBar = {
            WaddleTopBar(
                title = { Text(text = stringResource(R.string.members_action)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Outlined.ArrowBack,
                            contentDescription = stringResource(R.string.action_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        MembersContent(
            state = state,
            roomName = name,
            filter = filter,
            actions = MembersListActions(
                onRowClick = { profileTarget = it },
                onFilter = { filter = it },
                onSearchQueryChanged = viewModel::onSearchQueryChanged,
                onAddMember = viewModel::addMember,
            ),
            contentPadding = padding,
            modifier = Modifier
                .consumeWindowInsets(padding)
                // Search results stay above the keyboard.
                .imePadding(),
        )
    }

    profileTarget?.let { row ->
        val selfJid = session?.jid?.let(::bareJidOf)
        val isSelf = row.jid != null && selfJid != null && normalizedBareJid(row.jid) == normalizedBareJid(selfJid)
        MemberProfileSheet(
            row = row,
            canManage = state.canManage && !row.inferred && !isSelf && row.affiliation != WaddleMucAffiliation.OWNER,
            onDismiss = { profileTarget = null },
            onMessage = row.jid?.takeIf { onOpenDm != null && !isSelf }?.let { jid ->
                {
                    profileTarget = null
                    onOpenDm?.invoke(jid, row.displayName)
                }
            },
            actions = MemberSheetActions(
                onSetAffiliation = { affiliation -> viewModel.setAffiliation(row, affiliation) },
                onKick = { pendingAction = PendingAction.Kick(row) },
                onBan = { pendingAction = PendingAction.Ban(row) },
                onRemove = { pendingAction = PendingAction.Remove(row) },
            ),
        )
    }

    pendingAction?.let { action ->
        ConfirmActionDialog(
            action = action,
            onConfirm = {
                when (action) {
                    is PendingAction.Kick -> viewModel.kick(action.row)
                    is PendingAction.Ban -> viewModel.ban(action.row)
                    is PendingAction.Remove -> viewModel.remove(action.row)
                }
                pendingAction = null
            },
            onDismiss = { pendingAction = null },
        )
    }
}

/**
 * Stateless member list: header (room kicker, count, lead, Here now /
 * Everyone), the add-member search for managers, member cards, bots.
 */
@Composable
internal fun MembersContent(
    state: MembersUiState,
    roomName: String,
    filter: MemberFilter,
    actions: MembersListActions,
    modifier: Modifier = Modifier,
    contentPadding: PaddingValues = PaddingValues(),
) {
    val hereNow = state.rows.filter { it.presentNow }
    val shown = if (filter == MemberFilter.HERE_NOW) hereNow else state.rows
    val loading = state.status == MemberListStatus.LOADING && state.rows.isEmpty()
    LazyColumn(
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        contentPadding = PaddingValues(
            top = contentPadding.calculateTopPadding(),
            bottom = contentPadding.calculateBottomPadding() + Waddle.space.xl,
        ),
        modifier = modifier,
    ) {
        item(key = "header") {
            MembersHeader(
                roomName = roomName,
                total = state.rows.size,
                here = hereNow.size,
                filter = filter,
                onFilter = actions.onFilter,
            )
        }
        if (state.canManage) {
            addMemberItems(state, actions)
        }
        if (state.status == MemberListStatus.UNAVAILABLE) {
            item(key = "status") { StatusLine(rowsEmpty = state.rows.isEmpty()) }
        }
        when {
            loading -> items(count = SKELETON_CARDS, key = { "skeleton:$it" }) { MemberCardSkeleton() }
            shown.isEmpty() && state.status != MemberListStatus.UNAVAILABLE -> item(key = "empty") {
                EmptyState(
                    icon = Icons.Outlined.Group,
                    title = stringResource(R.string.huddle_members_empty_title),
                    body = stringResource(
                        if (filter == MemberFilter.HERE_NOW && state.rows.isNotEmpty()) {
                            R.string.huddle_members_empty_here
                        } else {
                            R.string.huddle_members_empty_everyone
                        },
                    ),
                )
            }
            else -> items(items = shown, key = { "${it.jid ?: it.displayName}:${it.inferred}" }) { row ->
                MemberCard(row = row, onClick = { actions.onRowClick(row) })
            }
        }
        if (filter == MemberFilter.EVERYONE && state.bots.isNotEmpty()) {
            item(key = "bots-header") {
                SectionKicker(text = stringResource(R.string.members_bots_section))
            }
            items(items = state.bots, key = { "bot:${it.jid}" }) { bot -> BotCard(bot) }
        }
    }
}

@Composable
private fun MembersHeader(
    roomName: String,
    total: Int,
    here: Int,
    filter: MemberFilter,
    onFilter: (MemberFilter) -> Unit,
) {
    val colors = Waddle.colors
    Column(
        verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md)
            .padding(top = Waddle.space.xs, bottom = Waddle.space.sm),
    ) {
        if (roomName.isNotEmpty()) {
            KickerText(text = stringResource(R.string.huddle_members_in_room, roomName))
        }
        Text(
            text = pluralStringResource(R.plurals.huddle_members_count, total, total),
            style = MaterialTheme.typography.headlineMedium,
            color = colors.fg,
            modifier = Modifier.semantics { heading() },
        )
        Text(
            text = pluralStringResource(R.plurals.huddle_members_lead, here, here),
            style = Waddle.type.body,
            color = colors.fgMuted,
        )
        SegmentedChoice(
            options = listOf(
                ChoiceOption(MemberFilter.HERE_NOW, stringResource(R.string.huddle_members_filter_here)),
                ChoiceOption(MemberFilter.EVERYONE, stringResource(R.string.huddle_members_filter_everyone)),
            ),
            selected = filter,
            onSelect = onFilter,
            modifier = Modifier.padding(top = Waddle.space.sm),
        )
    }
}

private fun LazyListScope.addMemberItems(state: MembersUiState, actions: MembersListActions) {
    item(key = "search") {
        OutlinedTextField(
            value = state.searchQuery,
            onValueChange = actions.onSearchQueryChanged,
            placeholder = { Text(text = stringResource(R.string.members_search_hint)) },
            leadingIcon = { Icon(Icons.Outlined.PersonAdd, contentDescription = null) },
            singleLine = true,
            shape = HuddleFieldShape,
            colors = huddleFieldColors(),
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Waddle.space.md)
                .testTag(MembersScreenTestTags.SEARCH_FIELD),
        )
    }
    items(items = state.searchResults, key = { "result:${it.jid}" }) { entry ->
        val label = entry.displayName ?: entry.username
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = RESULT_ROW)
                .padding(horizontal = Waddle.space.md),
        ) {
            HuddleAvatar(jid = entry.jid, name = label, size = AvatarSize.MD)
            Column(modifier = Modifier.weight(1f)) {
                Text(text = label, style = Waddle.type.body, color = Waddle.colors.fg, maxLines = 1)
                Text(
                    text = entry.jid,
                    style = Waddle.type.time,
                    color = Waddle.colors.fgMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            PillButton(
                text = stringResource(R.string.members_add_as_member),
                onClick = { actions.onAddMember(entry) },
                tone = PillTone.QUIET,
            )
        }
    }
}

@Composable
private fun StatusLine(rowsEmpty: Boolean) {
    Text(
        text = stringResource(if (rowsEmpty) R.string.members_unavailable else R.string.members_last_synced),
        style = Waddle.type.control,
        color = Waddle.colors.warningText,
        modifier = Modifier.padding(horizontal = Waddle.space.md, vertical = Waddle.space.xxs),
    )
}

/** One member: avatar with presence and huddle ring, name, role kicker, where they are. */
@Composable
private fun MemberCard(row: MemberRow, onClick: () -> Unit) {
    val colors = Waddle.colors
    HuddleCard(
        onClick = onClick,
        contentPadding = PaddingValues(horizontal = Waddle.space.md, vertical = Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md)
            .testTag(MembersScreenTestTags.MEMBER_ROW_PREFIX + row.displayName),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        ) {
            HuddleAvatar(
                jid = row.jid,
                name = row.displayName,
                size = AvatarSize.LG,
                presence = row.presence.takeIf { row.presentNow || it != null },
                inHuddle = row.inHuddle,
                ground = colors.surface,
            )
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
                Text(
                    text = row.displayName,
                    style = MaterialTheme.typography.titleMedium,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                RoleLine(affiliation = row.affiliation, hats = row.hats)
                Text(
                    text = statusOf(row),
                    style = Waddle.type.control.copy(fontWeight = null),
                    color = if (row.inHuddle) colors.liveText else colors.fgMuted,
                    maxLines = 1,
                )
            }
        }
    }
}

/** Affiliation and hats as mono kickers: owner gold, admin teal, banned danger. */
@Composable
private fun RoleLine(affiliation: WaddleMucAffiliation, hats: List<String>) {
    val colors = Waddle.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
    ) {
        KickerText(text = affiliationLabel(affiliation), color = affiliationColor(affiliation))
        hats.forEach { hat -> KickerText(text = hat, color = colors.actionText) }
    }
}

@Composable
private fun affiliationColor(affiliation: WaddleMucAffiliation): Color {
    val colors = Waddle.colors
    return when (affiliation) {
        WaddleMucAffiliation.OWNER -> colors.goldText
        WaddleMucAffiliation.ADMIN -> colors.actionText
        WaddleMucAffiliation.OUTCAST -> colors.dangerText
        WaddleMucAffiliation.MEMBER, WaddleMucAffiliation.NONE -> colors.fgMuted
    }
}

/** Where someone is, in words (web member status line). */
@Composable
private fun statusOf(row: MemberRow): String = stringResource(
    when {
        row.inHuddle -> R.string.huddle_member_in_huddle
        row.presentNow -> when (row.presence) {
            PresenceShow.AWAY, PresenceShow.XA -> R.string.huddle_member_here_away
            PresenceShow.DND -> R.string.huddle_member_here_dnd
            else -> R.string.huddle_member_here
        }
        row.presence != null && row.presence != PresenceShow.OFFLINE -> R.string.huddle_member_elsewhere
        else -> R.string.huddle_member_not_here
    },
)

@Composable
private fun BotCard(bot: BotRow) {
    val colors = Waddle.colors
    HuddleCard(
        contentPadding = PaddingValues(horizontal = Waddle.space.md, vertical = Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        ) {
            HuddleAvatar(jid = bot.jid, name = bot.displayName, size = AvatarSize.LG, ground = colors.surface)
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
                Text(
                    text = bot.displayName,
                    style = MaterialTheme.typography.titleMedium,
                    color = colors.fg,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                KickerText(text = stringResource(R.string.huddle_member_bot), color = colors.mossText)
            }
        }
    }
}

@Composable
private fun MemberCardSkeleton() {
    HuddleCard(
        contentPadding = PaddingValues(horizontal = Waddle.space.md, vertical = Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        ) {
            SkeletonBlock(
                height = AvatarSize.LG.dp,
                radius = Waddle.radius.pill,
                modifier = Modifier.size(AvatarSize.LG.dp),
            )
            Column(modifier = Modifier.weight(1f)) {
                SkeletonBlock(modifier = Modifier.width(SKELETON_NAME))
                SkeletonBlock(modifier = Modifier.width(SKELETON_ROLE))
            }
        }
    }
}

/** Wire value stays `outcast`; only the label reads "Banned" (web parity). */
@Composable
private fun affiliationLabel(affiliation: WaddleMucAffiliation): String = stringResource(
    when (affiliation) {
        WaddleMucAffiliation.OWNER -> R.string.affiliation_owner
        WaddleMucAffiliation.ADMIN -> R.string.affiliation_admin
        WaddleMucAffiliation.MEMBER -> R.string.affiliation_member
        WaddleMucAffiliation.OUTCAST -> R.string.affiliation_banned
        WaddleMucAffiliation.NONE -> R.string.affiliation_none
    },
)

/** The owner/admin verbs the profile sheet offers. */
private data class MemberSheetActions(
    val onSetAffiliation: (WaddleMucAffiliation) -> Unit,
    val onKick: () -> Unit,
    val onBan: () -> Unit,
    val onRemove: () -> Unit,
)

/**
 * A member's profile (web member profile sheet): who they are, where
 * they are — a glowing ember banner while in the huddle — "Message" when
 * a DM can be opened, and the management verbs for owners and admins.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MemberProfileSheet(
    row: MemberRow,
    canManage: Boolean,
    onDismiss: () -> Unit,
    onMessage: (() -> Unit)?,
    actions: MemberSheetActions,
) {
    val colors = Waddle.colors
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = colors.surface) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(bottom = Waddle.space.md),
        ) {
            KickerText(text = stringResource(R.string.huddle_member_profile))
            HuddleAvatar(
                jid = row.jid,
                name = row.displayName,
                size = AvatarSize.XL,
                presence = row.presence,
                inHuddle = row.inHuddle,
                ground = colors.surface,
                modifier = Modifier.padding(vertical = Waddle.space.sm),
            )
            Text(
                text = row.displayName,
                style = MaterialTheme.typography.headlineSmall,
                color = colors.fg,
                textAlign = TextAlign.Center,
                modifier = Modifier
                    .padding(horizontal = Waddle.space.md)
                    .semantics { heading() },
            )
            row.jid?.let {
                Text(text = it, style = Waddle.type.time, color = colors.fgMuted, maxLines = 1)
            }
            Row(modifier = Modifier.padding(top = Waddle.space.xxs)) {
                RoleLine(affiliation = row.affiliation, hats = row.hats)
            }
            if (row.inHuddle) {
                HuddleBanner(modifier = Modifier.padding(top = Waddle.space.sm))
            } else {
                Text(text = statusOf(row), style = Waddle.type.control, color = colors.fgMuted)
            }
            onMessage?.let {
                PillButton(
                    text = stringResource(R.string.huddle_member_message),
                    onClick = it,
                    icon = Icons.Outlined.ChatBubbleOutline,
                    modifier = Modifier.padding(top = Waddle.space.md),
                )
            }
        }
        if (canManage) {
            ManageActions(row = row, onDismiss = onDismiss, actions = actions)
        }
    }
}

@Composable
private fun HuddleBanner(modifier: Modifier = Modifier) {
    val colors = Waddle.colors
    val shape = RoundedCornerShape(Waddle.radius.panel)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        modifier = modifier
            .padding(horizontal = Waddle.space.md)
            .glow(colors.glowLive, blur = BANNER_GLOW, cornerRadius = Waddle.radius.panel, spread = BANNER_GLOW_SPREAD)
            .background(colors.liveSubtle, shape)
            .border(BorderStroke(1.dp, colors.live), shape)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xs),
    ) {
        LiveDot()
        Text(
            text = stringResource(R.string.huddle_member_in_huddle),
            style = Waddle.type.control,
            color = colors.liveText,
        )
    }
}

@Composable
private fun ManageActions(row: MemberRow, onDismiss: () -> Unit, actions: MemberSheetActions) {
    val danger = Waddle.colors.dangerText
    HorizontalDivider(color = Waddle.colors.borderSoft, modifier = Modifier.padding(top = Waddle.space.xs))
    SectionKicker(text = stringResource(R.string.huddle_member_manage))
    Column(modifier = Modifier.navigationBarsPadding().padding(bottom = Waddle.space.xs)) {
        if (row.affiliation != WaddleMucAffiliation.ADMIN) {
            GroupedRow(title = stringResource(R.string.members_make_admin), onClick = {
                actions.onSetAffiliation(WaddleMucAffiliation.ADMIN)
                onDismiss()
            })
        }
        if (row.affiliation != WaddleMucAffiliation.MEMBER) {
            GroupedRow(title = stringResource(R.string.members_make_member), onClick = {
                actions.onSetAffiliation(WaddleMucAffiliation.MEMBER)
                onDismiss()
            })
        }
        if (row.presentNow && row.nick != null) {
            GroupedRow(title = stringResource(R.string.members_kick), titleColor = danger, onClick = {
                actions.onKick()
                onDismiss()
            })
        }
        if (row.affiliation != WaddleMucAffiliation.OUTCAST) {
            GroupedRow(title = stringResource(R.string.members_ban), titleColor = danger, onClick = {
                actions.onBan()
                onDismiss()
            })
        }
        GroupedRow(title = stringResource(R.string.members_remove), titleColor = danger, onClick = {
            actions.onRemove()
            onDismiss()
        })
    }
}

@Composable
private fun ConfirmActionDialog(
    action: PendingAction,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val (titleRes, bodyRes, confirmRes) = when (action) {
        is PendingAction.Kick -> Triple(
            R.string.members_kick_confirm_title,
            R.string.members_kick_confirm_body,
            R.string.members_kick,
        )
        is PendingAction.Ban -> Triple(
            R.string.members_ban_confirm_title,
            R.string.members_ban_confirm_body,
            R.string.members_ban,
        )
        is PendingAction.Remove -> Triple(
            R.string.members_remove_confirm_title,
            R.string.members_remove_confirm_body,
            R.string.members_remove,
        )
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = Waddle.colors.surface,
        title = { Text(text = stringResource(titleRes)) },
        text = { Text(text = stringResource(bodyRes, action.row.displayName), color = Waddle.colors.fgMuted) },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(text = stringResource(confirmRes), color = Waddle.colors.dangerText)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(text = stringResource(R.string.action_cancel), color = Waddle.colors.fg)
            }
        },
    )
}

private const val SKELETON_CARDS = 4
private val SKELETON_NAME = 120.dp
private val SKELETON_ROLE = 72.dp
private val RESULT_ROW = 56.dp
private val BANNER_GLOW = 28.dp
private val BANNER_GLOW_SPREAD = (-10).dp

/** Semantics tags shared with instrumented tests. */
object MembersScreenTestTags {
    const val SEARCH_FIELD = "members-search-field"
    const val MEMBER_ROW_PREFIX = "member-row:"
}
