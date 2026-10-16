package social.waddle.android.screenshots

import android.app.Application
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.feature.conversation.ComposerMode
import social.waddle.android.feature.conversation.MessageComposer

/** The composer card: empty, and replying with a draft. */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h320dp-xxhdpi", application = Application::class)
class ComposerScreenshotTest {
    @Test
    fun composer() = captureThemed("composer") {
        Column(verticalArrangement = Arrangement.spacedBy(16.dp, androidx.compose.ui.Alignment.Bottom)) {
            MessageComposer(onSend = { _, _ -> }, onAttach = {}, onGif = {}, onSticker = {})
            MessageComposer(
                onSend = { _, _ -> },
                onAttach = {},
                replying = ComposerMode.Replying(
                    targetId = "m1",
                    authorJid = "alice@waddle.test",
                    authorName = "alice",
                    previewBody = "p99 is 41ms",
                    threadId = null,
                ),
                modifier = Modifier,
            )
        }
    }
}
