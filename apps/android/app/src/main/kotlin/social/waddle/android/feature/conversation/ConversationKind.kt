package social.waddle.android.feature.conversation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.res.stringResource
import social.waddle.android.R

/** What kind of conversation a screen shows: decides its empty state and composer placeholder. */
@Immutable
sealed interface ConversationKind {
    /** A room or group DM; [name] as the header shows it ("#general"). */
    data class Room(val name: String?) : ConversationKind

    /** A 1:1 conversation with [name]. */
    data class Dm(val name: String) : ConversationKind

    data object Thread : ConversationKind
}

/** The invitation an empty conversation shows (web empty-state copy). */
@Composable
fun TimelineEmpty(kind: ConversationKind) {
    when (kind) {
        is ConversationKind.Room -> ConversationEmptyState(
            title = kind.name?.let { stringResource(R.string.timeline_empty_room_title, it) }
                ?: stringResource(R.string.timeline_empty_room_title_unnamed),
            body = stringResource(R.string.timeline_empty_room_body),
        )
        is ConversationKind.Dm -> ConversationEmptyState(
            title = stringResource(R.string.timeline_empty_dm_title, kind.name),
            body = stringResource(R.string.timeline_empty_dm_body),
        )
        ConversationKind.Thread -> ConversationEmptyState(
            title = stringResource(R.string.timeline_empty_thread_title),
            body = stringResource(R.string.timeline_empty_thread_body),
        )
    }
}

/** "Message #general", "Message alice", "Reply in thread". */
@Composable
fun composerPlaceholderOf(kind: ConversationKind): String = when (kind) {
    is ConversationKind.Room -> kind.name?.let { stringResource(R.string.composer_placeholder_to, it) }
        ?: stringResource(R.string.composer_placeholder)
    is ConversationKind.Dm -> stringResource(R.string.composer_placeholder_to, kind.name)
    ConversationKind.Thread -> stringResource(R.string.composer_placeholder_thread)
}
