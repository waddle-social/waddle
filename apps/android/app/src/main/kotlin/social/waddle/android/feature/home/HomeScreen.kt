package social.waddle.android.feature.home

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.client.CreateRoomResult
import social.waddle.android.jid.localpartOf
import social.waddle.android.theme.MonoMeta
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.ConversationListRow
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.SectionKicker
import social.waddle.android.ui.WaddleTopBar

/**
 * The Home tab: the community's channels by space, a filter, unread
 * state and the connection banner. Direct messages, activity and
 * settings are sibling tabs, not a drawer.
 */
@Composable
fun HomeScreen(
    onOpenChannel: (roomJid: String, name: String) -> Unit,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
) {
    val graph = LocalAppGraph.current
    val viewModel: HomeViewModel = viewModel(factory = HomeViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val canCreateChannel by viewModel.canCreateChannel.collectAsStateWithLifecycle()
    var createDialogOpen by remember { mutableStateOf(false) }
    var creating by remember { mutableStateOf(false) }
    var filter by rememberSaveable { mutableStateOf("") }
    val snackbarHostState = remember { SnackbarHostState() }
    val createFailedText = stringResource(R.string.create_channel_failed)
    val createNotPermittedText = stringResource(R.string.create_channel_not_permitted)

    fun openChannel(channel: ChannelListItem) {
        viewModel.openChannel(channel.roomJid)
        onOpenChannel(channel.roomJid, channel.name)
    }

    LaunchedEffect(viewModel) {
        viewModel.createEvents.collect { result ->
            creating = false
            when (result) {
                is CreateRoomResult.Created -> {
                    createDialogOpen = false
                    viewModel.openChannel(result.roomJid)
                    onOpenChannel(result.roomJid, localpartOf(result.roomJid))
                }
                CreateRoomResult.NotPermitted ->
                    snackbarHostState.showSnackbar(createNotPermittedText)
                else -> snackbarHostState.showSnackbar(createFailedText)
            }
        }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbarHostState) },
        contentWindowInsets = contentWindowInsets,
        topBar = {
            WaddleTopBar(
                title = { Text(text = stringResource(R.string.app_name)) },
                actions = {
                    // Web parity: the "+" is owner-gated (canManageChannels);
                    // creation is also re-authorized server-side.
                    if (canCreateChannel) {
                        IconButton(
                            onClick = { createDialogOpen = true },
                            modifier = Modifier.testTag(HomeScreenTestTags.CREATE_CHANNEL_ACTION),
                        ) {
                            Icon(Icons.Outlined.Add, contentDescription = stringResource(R.string.create_channel_title))
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .consumeWindowInsets(padding)
                .fillMaxSize(),
        ) {
            ConnectionBanner(state = state.connectionState, onRetry = viewModel::retryConnection)
            ChannelListBody(
                sections = state.sections,
                filter = filter,
                onFilterChange = { filter = it },
                onChannelClick = ::openChannel,
            )
        }
    }

    if (createDialogOpen) {
        CreateChannelDialog(
            onDismiss = { createDialogOpen = false },
            onCreate = { name, description, forum ->
                creating = true
                viewModel.createChannel(name, description, forum)
            },
            creating = creating,
        )
    }
}

/** Semantics tags shared with instrumented tests. */
object HomeScreenTestTags {
    const val CREATE_CHANNEL_ACTION = "home-create-channel-action"
    const val CHANNEL_CALL_BADGE = "home-channel-call-badge"
    const val FILTER_FIELD = "home-channel-filter"
}

@Composable
internal fun ChannelListBody(
    sections: List<SpaceSection>,
    filter: String,
    onFilterChange: (String) -> Unit,
    onChannelClick: (ChannelListItem) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (sections.all { it.channels.isEmpty() }) {
        EmptyState(
            icon = Icons.Outlined.Tag,
            title = stringResource(R.string.home_empty_title),
            body = stringResource(R.string.home_empty),
            modifier = modifier,
        )
        return
    }
    val query = filter.trim()
    val visible = if (query.isEmpty()) {
        sections
    } else {
        sections.map { section ->
            section.copy(channels = section.channels.filter { it.name.contains(query, ignoreCase = true) })
        }
    }
    LazyColumn(modifier = modifier.fillMaxSize()) {
        item(key = "filter") { ChannelFilterField(value = filter, onValueChange = onFilterChange) }
        visible.forEach { section ->
            if (section.channels.isEmpty()) return@forEach
            item(key = "header:${section.id}", contentType = "header") {
                SectionKicker(text = section.name ?: stringResource(R.string.home_section_channels))
            }
            items(items = section.channels, key = { "channel:${it.roomJid}" }, contentType = { "channel" }) { channel ->
                ConversationListRow(
                    title = channel.name,
                    leading = {
                        Icon(
                            Icons.Outlined.Tag,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(20.dp),
                        )
                    },
                    onClick = { onChannelClick(channel) },
                    unreadCount = channel.unreadCount,
                    isMuted = channel.isMuted,
                    conversationJid = channel.roomJid,
                    trailingExtra = { CallBadge(inCallCount = channel.inCallCount) },
                )
            }
        }
        if (visible.all { it.channels.isEmpty() }) {
            item(key = "no-match") {
                Text(
                    text = stringResource(R.string.home_filter_no_match, query),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(Waddle.space.md),
                )
            }
        }
        item(key = "bottom-space") { Spacer(Modifier.height(Waddle.space.lg)) }
    }
}

/** Pill filter over the channel list: the mobile quick switcher. */
@Composable
private fun ChannelFilterField(value: String, onValueChange: (String) -> Unit) {
    val textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface)
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        singleLine = true,
        textStyle = textStyle,
        cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.xxs)
            .testTag(HomeScreenTestTags.FILTER_FIELD),
        decorationBox = { inner ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
                modifier = Modifier
                    .background(MaterialTheme.colorScheme.surfaceContainerHigh, CircleShape)
                    .padding(horizontal = Waddle.space.md, vertical = 10.dp),
            ) {
                Icon(
                    Icons.Outlined.Search,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(18.dp),
                )
                Box(Modifier.weight(1f)) {
                    if (value.isEmpty()) {
                        Text(
                            text = stringResource(R.string.home_filter_placeholder),
                            style = textStyle,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    inner()
                }
            }
        },
    )
}

/** XEP-0272 live-call chip: ember, because it is happening now. */
@Composable
private fun CallBadge(inCallCount: Int) {
    if (inCallCount <= 0) return
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(2.dp),
        modifier = Modifier.testTag(HomeScreenTestTags.CHANNEL_CALL_BADGE),
    ) {
        Icon(
            Icons.Outlined.Call,
            contentDescription = stringResource(R.string.home_channel_call_badge),
            tint = Waddle.colors.liveText,
            modifier = Modifier.size(16.dp),
        )
        Text(text = inCallCount.toString(), style = MonoMeta, color = Waddle.colors.liveText)
    }
}
