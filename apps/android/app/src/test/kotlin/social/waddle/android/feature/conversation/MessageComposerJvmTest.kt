package social.waddle.android.feature.conversation

import android.app.Application
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.text.AnnotatedString
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.waddle.android.client.MentionCandidate
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.theme.WaddleTheme

/**
 * The composer's text field runs on a TextFieldState while the composer
 * keeps a TextFieldValue draft: typing must reach the draft, and drafts
 * the composer sets itself (send, edit, the @ button) must reach the field.
 */
@RunWith(AndroidJUnit4::class)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class MessageComposerJvmTest {
    @get:Rule
    val compose = createComposeRule()

    private val field get() = compose.onNode(hasSetTextAction())

    private fun themed(content: @Composable () -> Unit) = compose.setContent {
        WaddleTheme(themeMode = ThemeMode.DAYLIGHT, content = content)
    }

    @Test
    fun `typed text sends and the cleared draft empties the field`() {
        val sent = mutableListOf<String>()
        themed { MessageComposer(onSend = { body, _ -> sent += body }) }

        compose.onNodeWithContentDescription("Send").assertIsNotEnabled()
        field.performTextInput("hello penguins")
        compose.onNodeWithContentDescription("Send").assertIsEnabled().performClick()

        compose.runOnIdle { assertEquals(listOf("hello penguins"), sent) }
        // Text also carries the placeholder; the draft itself is empty.
        field.assert(SemanticsMatcher.expectValue(SemanticsProperties.EditableText, AnnotatedString("")))
        compose.onNodeWithContentDescription("Send").assertIsNotEnabled()
    }

    @Test
    fun `an edit fills the field and ending it restores what was being typed`() {
        val editing = mutableStateOf<ComposerMode.Editing?>(null)
        themed {
            MessageComposer(onSend = { _, _ -> }, editing = editing.value, onCancelEdit = { editing.value = null })
        }

        field.performTextInput("draft in progress")
        compose.runOnIdle { editing.value = ComposerMode.Editing(targetId = "t1", originalBody = "original body") }
        field.assertTextEquals("original body")

        compose.onNodeWithContentDescription("Cancel edit").performClick()
        field.assertTextEquals("draft in progress")
    }

    @Test
    fun `typing continues after text the composer inserted`() {
        themed {
            MessageComposer(
                onSend = { _, _ -> },
                mentionCandidates = listOf(MentionCandidate("alice", "xmpp:alice@waddle.test", false)),
            )
        }

        field.performTextInput("hi ")
        compose.onNodeWithContentDescription("Mention someone").performClick()
        field.performTextInput("al")

        field.assertTextEquals("hi @al")
    }
}
