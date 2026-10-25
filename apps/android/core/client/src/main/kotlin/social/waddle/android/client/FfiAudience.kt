package social.waddle.android.client

import social.waddle.client.ffi.WaddleGroupDmHistoryAccess
import social.waddle.client.ffi.WaddleMessageAudience

/** `true` is a MUC (`type='groupchat'`). `false` is a direct chat. */
internal fun messageAudience(isGroupchat: Boolean): WaddleMessageAudience =
    if (isGroupchat) WaddleMessageAudience.MUC else WaddleMessageAudience.DIRECT

/** `true` grants the full archive. `false` is the from-join default. */
internal fun groupDmHistory(fullHistory: Boolean): WaddleGroupDmHistoryAccess =
    if (fullHistory) WaddleGroupDmHistoryAccess.FULL else WaddleGroupDmHistoryAccess.FROM_JOIN
