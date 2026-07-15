package social.waddle.android.feature.home

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material3.Badge
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.NavigationDrawerItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberDrawerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.launch
import social.waddle.android.LocalAppGraph
import social.waddle.android.R

/**
 * Home: spaces→channels drawer + channel list body, unread badges, DM
 * and Settings entries, connection banner.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    onOpenChannel: (roomJid: String, name: String) -> Unit,
    onOpenDmList: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val graph = LocalAppGraph.current
    val viewModel: HomeViewModel = viewModel(factory = HomeViewModel.factory(graph))
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    val drawerState = rememberDrawerState(DrawerValue.Closed)
    val scope = rememberCoroutineScope()

    fun closeDrawerAnd(action: () -> Unit) {
        scope.launch { drawerState.close() }
        action()
    }

    fun openChannel(channel: ChannelListItem) {
        viewModel.openChannel(channel.roomJid)
        onOpenChannel(channel.roomJid, channel.name)
    }

    ModalNavigationDrawer(
        drawerState = drawerState,
        drawerContent = {
            ModalDrawerSheet {
                DrawerContent(
                    state = state,
                    onChannelClick = { channel -> closeDrawerAnd { openChannel(channel) } },
                    onOpenDmList = { closeDrawerAnd(onOpenDmList) },
                    onOpenSettings = { closeDrawerAnd(onOpenSettings) },
                )
            }
        },
    ) {
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(text = stringResource(R.string.app_name)) },
                    navigationIcon = {
                        IconButton(onClick = { scope.launch { drawerState.open() } }) {
                            Icon(
                                Icons.Default.Menu,
                                contentDescription = stringResource(R.string.home_open_menu),
                            )
                        }
                    },
                )
            },
        ) { padding ->
            Column(
                modifier = Modifier
                    .padding(padding)
                    .fillMaxSize(),
            ) {
                ConnectionBanner(state = state.connectionState)
                ChannelListBody(sections = state.sections, onChannelClick = ::openChannel)
            }
        }
    }
}

@Composable
private fun DrawerContent(
    state: HomeUiState,
    onChannelClick: (ChannelListItem) -> Unit,
    onOpenDmList: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    LazyColumn {
        state.sections.forEach { section ->
            item(key = "header:${section.id}") {
                SectionHeader(name = section.name ?: stringResource(R.string.home_section_channels))
            }
            items(items = section.channels, key = { "drawer:${it.roomJid}" }) { channel ->
                NavigationDrawerItem(
                    label = {
                        Text(text = stringResource(R.string.home_channel_label, channel.name))
                    },
                    selected = false,
                    onClick = { onChannelClick(channel) },
                    icon = { Icon(Icons.Outlined.Tag, contentDescription = null) },
                    badge = { UnreadBadge(count = channel.unreadCount) },
                    modifier = Modifier.padding(horizontal = 12.dp),
                )
            }
        }
        item(key = "drawer-divider") {
            HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
        }
        item(key = "drawer-dms") {
            NavigationDrawerItem(
                label = { Text(text = stringResource(R.string.home_direct_messages)) },
                selected = false,
                onClick = onOpenDmList,
                icon = { Icon(Icons.Outlined.ChatBubbleOutline, contentDescription = null) },
                badge = { UnreadBadge(count = state.dmUnreadCount) },
                modifier = Modifier.padding(horizontal = 12.dp),
            )
        }
        item(key = "drawer-settings") {
            NavigationDrawerItem(
                label = { Text(text = stringResource(R.string.home_settings)) },
                selected = false,
                onClick = onOpenSettings,
                icon = { Icon(Icons.Outlined.Settings, contentDescription = null) },
                modifier = Modifier.padding(horizontal = 12.dp),
            )
        }
    }
}

@Composable
private fun ChannelListBody(
    sections: List<SpaceSection>,
    onChannelClick: (ChannelListItem) -> Unit,
) {
    if (sections.all { it.channels.isEmpty() }) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(32.dp),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = stringResource(R.string.home_empty),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
        return
    }
    LazyColumn {
        sections.forEach { section ->
            if (section.channels.isEmpty()) return@forEach
            item(key = "body-header:${section.id}") {
                SectionHeader(name = section.name ?: stringResource(R.string.home_section_channels))
            }
            items(items = section.channels, key = { "body:${it.roomJid}" }) { channel ->
                ListItem(
                    headlineContent = {
                        Text(text = stringResource(R.string.home_channel_label, channel.name))
                    },
                    leadingContent = { Icon(Icons.Outlined.Tag, contentDescription = null) },
                    trailingContent = { UnreadBadge(count = channel.unreadCount) },
                    modifier = Modifier.clickable { onChannelClick(channel) },
                )
            }
        }
    }
}

@Composable
private fun SectionHeader(name: String) {
    Text(
        text = name,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, end = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun UnreadBadge(count: Int) {
    if (count > 0) {
        Badge { Text(text = count.toString()) }
    }
}
