package social.waddle.android.feature.conversation

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.android.client.testPresence
import social.waddle.client.ffi.WaddleLinkPreview
import social.waddle.client.ffi.WaddleMarkupSpan
import social.waddle.client.ffi.WaddleMarkupSpanType
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddleSharedFile

/** Rich-content rendering in MessageCard: markup, previews, stickers, badges. */
@RunWith(AndroidJUnit4::class)
class MessageCardRichContentTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private fun itemOf(message: WaddleMessage): TimelineItem = TimelineItem(
        id = message.id ?: "msg-1",
        conversationJid = "room@muc.waddle.test",
        from = message.from,
        body = message.body.orEmpty(),
        timestamp = null,
        isMine = false,
        source = TimelineSource.Live(message),
    )

    private fun setCard(
        message: WaddleMessage,
        authorPresence: Map<String, WaddlePresence> = emptyMap(),
    ) {
        composeRule.setContent {
            MessageCard(
                row = ConversationRow.Stored(itemOf(message)),
                onRetry = {},
                authorPresence = authorPresence,
                trustedMediaOrigin = "https://xmpp.waddle.test",
            )
        }
    }

    @Test
    fun markupCodeBlockPartitionsTheBody() {
        setCard(
            testMessage(
                body = "before\ncode line\nafter",
                markupSpans = listOf(
                    WaddleMarkupSpan(
                        spanType = WaddleMarkupSpanType.CODE_BLOCK,
                        start = 7u,
                        end = 16u,
                        uri = null,
                    ),
                ),
            ),
        )
        composeRule.onNodeWithText("before").assertExists()
        composeRule.onNodeWithText("code line").assertExists()
        composeRule.onNodeWithText("after").assertExists()
    }

    @Test
    fun styledParagraphStillRendersItsFullText() {
        setCard(
            testMessage(
                body = "make it bold",
                markupSpans = listOf(
                    WaddleMarkupSpan(
                        spanType = WaddleMarkupSpanType.BOLD,
                        start = 8u,
                        end = 12u,
                        uri = null,
                    ),
                ),
            ),
        )
        composeRule.onNodeWithText("make it bold").assertExists()
    }

    @Test
    fun linkPreviewCardShowsHostTitleAndDescription() {
        setCard(
            testMessage(
                body = "https://example.com/story is neat",
                linkPreviews = listOf(
                    WaddleLinkPreview(
                        originalUrl = "https://example.com/story",
                        normalizedUrl = "https://example.com/story",
                        title = "An Article",
                        description = "Summary text",
                        image = null,
                        video = null,
                        playerEmbed = null,
                        remoteMediaUnavailable = false,
                    ),
                ),
            ),
        )
        composeRule.onNodeWithText("example.com").assertExists()
        composeRule.onNodeWithText("An Article").assertExists()
        composeRule.onNodeWithText("Summary text").assertExists()
    }

    @Test
    fun stickerSuppressesTheTextBubble() {
        setCard(
            testMessage(
                body = "penguin sticker",
                isSticker = true,
                sharedFiles = listOf(
                    WaddleSharedFile(
                        url = "https://xmpp.waddle.test/files/sticker.png",
                        name = "sticker.png",
                        mediaType = "image/png",
                        size = null,
                        width = null,
                        height = null,
                        desc = null,
                        hashes = emptyList(),
                        disposition = "inline",
                        encrypted = null,
                    ),
                ),
            ),
        )
        // The body is the sticker's alt text, never a text bubble.
        composeRule.onNodeWithText("penguin sticker").assertDoesNotExist()
        composeRule.onNodeWithContentDescription("penguin sticker").assertExists()
    }

    @Test
    fun ownerBadgeRendersNextToTheAuthor() {
        setCard(
            testMessage(
                from = "room@muc.waddle.test/alice",
                body = "hello",
                messageType = "groupchat",
                isMuc = true,
            ),
            authorPresence = mapOf(
                "alice" to testPresence(
                    from = "room@muc.waddle.test/alice",
                    mucAffiliation = WaddleMucAffiliation.OWNER,
                ),
            ),
        )
        composeRule.onNodeWithText("OWNER").assertExists()
    }
}
