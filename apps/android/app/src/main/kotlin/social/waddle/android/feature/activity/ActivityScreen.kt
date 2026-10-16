package social.waddle.android.feature.activity

import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.DoneAll
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import social.waddle.android.LocalAppGraph
import social.waddle.android.R
import social.waddle.android.avatar.LIST_AVATAR_SIZE
import social.waddle.android.avatar.PeerAvatar
import social.waddle.android.ui.ConversationListRow
import social.waddle.android.ui.EmptyState
import social.waddle.android.ui.GlyphTile
import social.waddle.android.ui.WaddleTopBar
import social.waddle.android.ui.listTimeOf
import java.time.Instant

/** The Activity tab: what is unread, across channels and DMs, newest first. */
@Composable
fun ActivityScreen(
    onOpenConversation: (jid: String) -> Unit,
    contentWindowInsets: WindowInsets = ScaffoldDefaults.contentWindowInsets,
) {
    val graph = LocalAppGraph.current
    val viewModel: ActivityViewModel = viewModel(factory = ActivityViewModel.factory(graph))
    val rows by viewModel.rows.collectAsStateWithLifecycle()
    Scaffold(
        contentWindowInsets = contentWindowInsets,
        topBar = { WaddleTopBar(title = { Text(text = stringResource(R.string.activity_title)) }) },
    ) { padding ->
        if (rows.isEmpty()) {
            EmptyState(
                icon = Icons.Outlined.DoneAll,
                title = stringResource(R.string.activity_empty_title),
                body = stringResource(R.string.activity_empty_body),
                modifier = Modifier.padding(padding),
            )
            return@Scaffold
        }
        LazyColumn(modifier = Modifier.padding(padding).consumeWindowInsets(padding)) {
            items(items = rows, key = { it.jid }, contentType = { "activity" }) { row ->
                ActivityListRow(row = row, onClick = { onOpenConversation(row.jid) })
            }
        }
    }
}

@Composable
private fun ActivityListRow(row: ActivityRow, onClick: () -> Unit) {
    ConversationListRow(
        title = if (row.kind == ActivityRow.Kind.CHANNEL) "#${row.name}" else row.name,
        leading = {
            when (row.kind) {
                ActivityRow.Kind.DIRECT -> PeerAvatar(jid = row.jid, displayName = row.name, size = LIST_AVATAR_SIZE)
                ActivityRow.Kind.GROUP_DM -> GlyphTile(Icons.Outlined.Group)
                ActivityRow.Kind.CHANNEL -> GlyphTile(Icons.Outlined.Tag)
            }
        },
        onClick = onClick,
        preview = row.preview,
        time = row.lastUpdated?.let { listTimeOf(Instant.ofEpochMilli(it)) },
        unreadCount = row.unreadCount,
        conversationJid = row.jid,
    )
}
