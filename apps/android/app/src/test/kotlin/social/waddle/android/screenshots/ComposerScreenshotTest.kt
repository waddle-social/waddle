package social.waddle.android.screenshots

import android.app.Application
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.ui.Alignment
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.feature.conversation.ComposerMode
import social.waddle.android.feature.conversation.MessageComposer
import social.waddle.android.feature.conversation.UploadState
import social.waddle.android.theme.Waddle

/** The composer: empty with a room placeholder, replying, and editing with a failed upload. */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h480dp-xxhdpi", application = Application::class)
class ComposerScreenshotTest {
    @Test
    fun composer() = captureThemed("composer") {
        ConversationFixtures.Pinned {
            Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.md, Alignment.Bottom)) {
                MessageComposer(
                    onSend = { _, _ -> },
                    onAttach = {},
                    onGif = {},
                    onSticker = {},
                    placeholder = "Message #general",
                )
                MessageComposer(
                    onSend = { _, _ -> },
                    onAttach = {},
                    replying = ComposerMode.Replying(
                        targetId = "m1",
                        authorJid = "alice@waddle.test",
                        authorName = "alice",
                        previewBody = "p99 is 41ms for a 50-message page.",
                        threadId = null,
                    ),
                    placeholder = "Message #general",
                )
                MessageComposer(
                    onSend = { _, _ -> },
                    onAttach = {},
                    uploadState = UploadState.TooLarge,
                    placeholder = "Message alice",
                )
            }
        }
    }
}
