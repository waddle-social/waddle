package social.waddle.android.feature.conversation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextOverflow
import social.waddle.android.R
import social.waddle.android.client.store.authorBareJidOf
import social.waddle.android.theme.Waddle
import social.waddle.android.ui.huddle.AvatarSize
import social.waddle.android.ui.huddle.HuddleAvatar

/**
 * The loaded threads of a conversation, newest activity first: root
 * author's face, the root's first line, and the reply count in teal with
 * the last reply's time. Tap opens the thread.
 */
@Composable
internal fun ThreadsOverviewSheet(
    threads: List<ThreadSummary>,
    selfBareJid: String?,
    onOpen: (threadId: String) -> Unit,
    onDismiss: () -> Unit,
) {
    ConversationSheet(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.threads_overview_title),
        contentPadding = Waddle.space.xs,
    ) {
        // Scrollable: long histories can hold more threads than the sheet's height.
        LazyColumn {
            items(threads, key = { it.threadId }) { thread ->
                ThreadSummaryRow(thread, selfBareJid) { onOpen(thread.threadId) }
            }
        }
    }
}

@Composable
private fun ThreadSummaryRow(thread: ThreadSummary, selfBareJid: String?, onClick: () -> Unit) {
    val colors = Waddle.colors
    val clock = rememberConversationClock()
    val author = thread.rootAuthor.orEmpty()
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = Waddle.space.xxxl)
            .clip(RoundedCornerShape(Waddle.radius.control))
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = Waddle.space.xs, vertical = Waddle.space.xs),
    ) {
        HuddleAvatar(
            jid = thread.rootItem?.let { authorBareJidOf(it, selfBareJid) },
            name = author,
            size = AvatarSize.MD,
            ground = colors.surface,
        )
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
            Text(
                text = if (thread.rootTombstoned) stringResource(R.string.message_deleted) else thread.rootPreview,
                style = Waddle.type.control,
                fontStyle = if (thread.rootTombstoned) FontStyle.Italic else null,
                color = colors.fg,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs)) {
                Text(
                    text = pluralStringResource(
                        R.plurals.thread_replies_count,
                        thread.replyCount,
                        thread.replyCount,
                        author,
                    ),
                    style = MetaStyle,
                    color = colors.actionText,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                thread.lastTimestamp?.let(::parseInstant)?.let { last ->
                    Text(text = clock.stamp(last), style = Waddle.type.time, color = colors.fgMuted, maxLines = 1)
                }
            }
        }
    }
}
