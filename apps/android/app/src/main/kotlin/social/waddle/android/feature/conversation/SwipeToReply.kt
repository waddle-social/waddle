package social.waddle.android.feature.conversation

import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp

/** A bounded horizontal drag; vertical scrolling and long-press keep their own gestures. */
@Composable
internal fun Modifier.swipeToReply(onReply: (() -> Unit)?): Modifier {
    if (onReply == null) return this
    val reply by rememberUpdatedState(onReply)
    val haptics = LocalHapticFeedback.current
    val threshold = with(LocalDensity.current) { REPLY_DISTANCE.toPx() }
    val direction = if (LocalLayoutDirection.current == LayoutDirection.Rtl) -1f else 1f
    var distance by remember { mutableFloatStateOf(0f) }
    return this
        .graphicsLayer { translationX = distance * direction }
        .pointerInput(threshold, direction) {
            detectHorizontalDragGestures(
                onDragEnd = {
                    if (distance >= threshold) {
                        haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                        reply()
                    }
                    distance = 0f
                },
                onDragCancel = { distance = 0f },
                onHorizontalDrag = { change, amount ->
                    distance = (distance + amount * direction).coerceIn(0f, threshold)
                    change.consume()
                },
            )
        }
}

private val REPLY_DISTANCE = 64.dp
