package social.waddle.android.feature.conversation

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import social.waddle.client.ffi.WaddleChatState

/**
 * XEP-0085 outbound state machine, web parity: the first keystroke in
 * an activity window sends `composing` (deduped while already
 * composing); each keystroke re-arms a 3s timer that sends `paused`
 * when typing stops; a successful message send cancels the timer and
 * sends `active`. Never emits `inactive`/`gone` (web parity).
 */
class ComposerTypingNotifier(
    private val scope: CoroutineScope,
    private val sendState: suspend (WaddleChatState) -> Unit,
    private val pauseMillis: Long = COMPOSING_PAUSE_MILLIS,
) {
    private var last = WaddleChatState.ACTIVE
    private var pauseJob: Job? = null

    fun onTyping() {
        if (last != WaddleChatState.COMPOSING) {
            last = WaddleChatState.COMPOSING
            scope.launch { sendState(WaddleChatState.COMPOSING) }
        }
        pauseJob?.cancel()
        pauseJob = scope.launch {
            delay(pauseMillis)
            last = WaddleChatState.PAUSED
            sendState(WaddleChatState.PAUSED)
        }
    }

    /** A message went out: typing ended with content, not a pause. */
    fun onMessageSent() {
        pauseJob?.cancel()
        pauseJob = null
        last = WaddleChatState.ACTIVE
        scope.launch { sendState(WaddleChatState.ACTIVE) }
    }

    private companion object {
        /** Web COMPOSING_PAUSE_MS parity. */
        const val COMPOSING_PAUSE_MILLIS = 3_000L
    }
}
