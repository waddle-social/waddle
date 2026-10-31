package social.waddle.android.flutter

import social.waddle.android.client.store.TimelineItem
import social.waddle.android.feature.conversation.ConversationRow
import social.waddle.android.feature.conversation.FeedEntry
import social.waddle.android.feature.conversation.threadIdFor

/** Explicit projections keep raw protocol records and decryption keys off platform channels. */
internal object FlutterConversationProjection {
    fun row(entry: FeedEntry, isGroup: Boolean): Map<String, Any?> {
        val row = entry.row
        val common =
            mapOf(
                "presentationId" to entry.key,
                "timestamp" to entry.sentAt.toString(),
                "daySeparator" to entry.daySeparator?.toString(),
                "unreadDivider" to entry.unreadDivider,
                "startsGroup" to entry.startsGroup,
                "replyCount" to entry.replyCount,
                "pinned" to entry.isPinned,
                "mentionsSelf" to entry.mentionsSelf,
                "isCallRow" to entry.isCallRow,
                "replyPreview" to replyPreview(entry, isGroup),
            )
        return common +
            when (row) {
                is ConversationRow.Stored -> messageMap(row.item, isGroup)
                is ConversationRow.Unconfirmed ->
                    with(row.message) {
                        mapOf(
                            "id" to "pending:$localId",
                            "localId" to localId,
                            "body" to body,
                            "mine" to true,
                            "author" to "You",
                            "pending" to true,
                            "failed" to failed,
                            "queued" to queued,
                            "acked" to acked,
                            "threadId" to extras?.threadId,
                            "replyTo" to extras?.replyToId,
                        )
                    }
            }
    }

    private fun messageMap(item: TimelineItem, isGroup: Boolean): Map<String, Any?> =
        with(item) {
            mapOf(
                "id" to id,
                "body" to if (tombstone == null) body else "Message removed",
                "author" to authorName(item, isGroup),
                "authorJid" to authorJid,
                "mine" to isMine,
                "threadId" to threadId,
                "replyTo" to replyToId,
                "threadTargetId" to threadIdFor(item, isGroup),
                "edited" to edited,
                "deleted" to (tombstone != null),
                "rejected" to rejected,
                "reactions" to
                    reactions.map {
                        mapOf("emoji" to it.emoji, "count" to it.count, "mine" to it.mine)
                    },
                "files" to
                    if (tombstone != null) {
                        emptyList<Any>()
                    } else {
                        sharedFiles.mapIndexed { index, file ->
                            mapOf(
                                "id" to index.toString(),
                                "name" to file.name,
                                "mimeType" to file.mediaType,
                                "url" to file.url.takeIf { file.encrypted == null },
                                "encrypted" to (file.encrypted != null),
                                "width" to file.width?.toLong(),
                                "height" to file.height?.toLong(),
                                "size" to file.size?.toLong(),
                            )
                        }
                    },
                "linkPreviews" to
                    if (tombstone != null) {
                        emptyList<Any>()
                    } else {
                        linkPreviews.map {
                            mapOf(
                                "url" to it.originalUrl,
                                "title" to it.title,
                                "description" to it.description,
                            )
                        }
                    },
                "sticker" to isSticker,
                "pending" to false,
            ) + richMetadata(item)
        }

    private fun replyPreview(entry: FeedEntry, isGroup: Boolean): Map<String, Any?>? {
        val parent = entry.replyParent
        if (parent != null) {
            val deleted = parent.tombstone != null
            return mapOf(
                "author" to authorName(parent, isGroup),
                "deleted" to deleted,
                "body" to
                    if (deleted) {
                        "Message removed"
                    } else {
                        parent.body
                    },
            )
        }
        return entry.replyFallbackQuote?.let { mapOf("body" to it, "deleted" to false) }
    }

    private fun authorName(item: TimelineItem, isGroup: Boolean): String? =
        when {
            item.isMine -> "You"
            isGroup -> item.from?.substringAfter('/')
            else -> item.from?.substringBefore('@')
        }

    private fun richMetadata(item: TimelineItem): Map<String, Any?> =
        with(item) {
            mapOf(
                "call" to
                    callAnchor?.let {
                        mapOf(
                            "kind" to it.kind,
                            "sid" to it.sid,
                            "media" to it.media,
                            "initiator" to it.initiator,
                            "started" to it.started,
                        )
                    },
                "callEnded" to
                    callEndedMarker?.let { mapOf("ended" to it.ended, "duration" to it.duration) },
                "markup" to
                    if (tombstone != null) {
                        emptyList<Any>()
                    } else {
                        markupSpans.map {
                            mapOf(
                                "start" to it.start.toLong(),
                                "end" to it.end.toLong(),
                                "kind" to it.spanType.name.lowercase(),
                                "uri" to it.uri,
                            )
                        }
                    },
                "safetyScores" to
                    safetyScores?.let { batch ->
                        mapOf(
                            "modelVersion" to batch.modelVersion,
                            "scores" to
                                batch.scores.map {
                                    mapOf(
                                        "category" to it.category.name.lowercase(),
                                        "probability" to it.probability,
                                        "taxonomyVersion" to it.taxonomyVersion,
                                    )
                                },
                        )
                    },
            )
        }
}
