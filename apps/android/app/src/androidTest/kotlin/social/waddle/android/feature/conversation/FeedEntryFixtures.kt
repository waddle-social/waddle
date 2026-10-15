package social.waddle.android.feature.conversation

import social.waddle.android.client.store.TimelineItem

/** One stored row as the feed would present it on its own. */
internal fun feedEntryOf(item: TimelineItem): FeedEntry =
    feedEntriesOf(
        items = listOf(item),
        pending = emptyList(),
        context = FeedContext(threadId = null, selfBareJid = null, pinnedIds = emptySet(), unreadAtOpen = 0),
    ).single()
