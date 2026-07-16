package social.waddle.android.feature.conversation

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.client.ffi.WaddleChatState

@OptIn(ExperimentalCoroutinesApi::class)
class ComposerTypingNotifierTest {
    @Test
    fun `first keystroke sends composing once and re-keys do not repeat`() = runTest {
        val sent = mutableListOf<WaddleChatState>()
        val notifier = ComposerTypingNotifier(backgroundScope, { sent += it })

        notifier.onTyping()
        runCurrent()
        notifier.onTyping()
        runCurrent()

        assertEquals(listOf(WaddleChatState.COMPOSING), sent)
    }

    @Test
    fun `three seconds of silence sends paused and typing again re-sends composing`() = runTest {
        val sent = mutableListOf<WaddleChatState>()
        val notifier = ComposerTypingNotifier(backgroundScope, { sent += it })

        notifier.onTyping()
        runCurrent()
        advanceTimeBy(3_001)
        runCurrent()
        assertEquals(listOf(WaddleChatState.COMPOSING, WaddleChatState.PAUSED), sent)

        notifier.onTyping()
        runCurrent()
        assertEquals(
            listOf(WaddleChatState.COMPOSING, WaddleChatState.PAUSED, WaddleChatState.COMPOSING),
            sent,
        )
    }

    @Test
    fun `each keystroke re-arms the pause timer`() = runTest {
        val sent = mutableListOf<WaddleChatState>()
        val notifier = ComposerTypingNotifier(backgroundScope, { sent += it })

        notifier.onTyping()
        advanceTimeBy(2_000)
        notifier.onTyping()
        advanceTimeBy(2_000)
        runCurrent()

        assertEquals(listOf(WaddleChatState.COMPOSING), sent)
    }

    @Test
    fun `send cancels the pause and emits active`() = runTest {
        val sent = mutableListOf<WaddleChatState>()
        val notifier = ComposerTypingNotifier(backgroundScope, { sent += it })

        notifier.onTyping()
        runCurrent()
        notifier.onMessageSent()
        runCurrent()
        advanceTimeBy(10_000)
        runCurrent()

        assertEquals(listOf(WaddleChatState.COMPOSING, WaddleChatState.ACTIVE), sent)
    }
}
