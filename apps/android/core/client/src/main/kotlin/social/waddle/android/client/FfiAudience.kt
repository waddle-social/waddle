package social.waddle.android.client

import social.waddle.client.ffi.WaddleGroupDmHistoryAccess
import social.waddle.client.ffi.WaddleInboxMessageContent
import social.waddle.client.ffi.WaddleInboxReadFilter
import social.waddle.client.ffi.WaddleMessageAudience

/** `true` is a MUC (`type='groupchat'`). `false` is a direct chat. */
internal fun messageAudience(isGroupchat: Boolean): WaddleMessageAudience =
    if (isGroupchat) WaddleMessageAudience.MUC else WaddleMessageAudience.DIRECT

/** `true` is unread-only. `false` is every conversation. */
internal fun inboxReadFilter(onlyUnread: Boolean): WaddleInboxReadFilter =
    if (onlyUnread) WaddleInboxReadFilter.UNREAD_ONLY else WaddleInboxReadFilter.ALL

/** `true` omits embedded messages. `false` includes them. */
internal fun inboxMessages(noMessages: Boolean): WaddleInboxMessageContent =
    if (noMessages) WaddleInboxMessageContent.OMIT else WaddleInboxMessageContent.INCLUDE

/** `true` grants the full archive. `false` is the from-join default. */
internal fun groupDmHistory(fullHistory: Boolean): WaddleGroupDmHistoryAccess =
    if (fullHistory) WaddleGroupDmHistoryAccess.FULL else WaddleGroupDmHistoryAccess.FROM_JOIN
