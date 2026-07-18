package social.waddle.android.client.prefs

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.FileDisposition
import social.waddle.android.client.MentionRef

class QueuedOutboundMessageTest {
    @Test
    fun `draft rejects a direct reply source for another target`() {
        val failure = runCatching {
            draft(
                payload = plainPayload(),
                source = DeliverySource.DirectReply(
                    conversationJid = "other@waddle.test",
                    isGroupchat = false,
                ),
            )
        }.exceptionOrNull()

        assertTrue(failure is IllegalArgumentException)
    }

    @Test
    fun `persisted decode rejects a direct reply source with another stanza kind`() {
        val message = draft(
            payload = plainPayload(),
            source = DeliverySource.DirectReply(
                conversationJid = PEER,
                isGroupchat = false,
            ),
        ).persisted(sequence = 1, ownership = OutboundOwnership.Ready)
        val encoded = JSON.encodeToJsonElement(
            QueuedOutboundMessage.serializer(),
            message,
        ).jsonObject
        val source = encoded.getValue("source").jsonObject
        val mismatched = JsonObject(
            encoded + (
                "source" to JsonObject(
                    source + ("isGroupchat" to JsonPrimitive(true)),
                )
            ),
        )

        val failure = runCatching {
            JSON.decodeFromJsonElement(QueuedOutboundMessage.serializer(), mismatched)
        }.exceptionOrNull()

        assertTrue(failure is IllegalArgumentException)
    }

    @Test
    fun `digest golden vectors preserve byte framing and annotation order`() {
        val annotated = QueuedOutboundPayload(
            target = QueuedOutboundTarget.Groupchat("general@muc.waddle.test"),
            content = QueuedOutboundContent(
                body = "reply",
                reply = QueuedOutboundReply(
                    id = "origin-1",
                    authorJid = null,
                    parentBody = "parent",
                ),
                thread = QueuedOutboundThread(
                    id = null,
                    parent = "thread-parent",
                ),
                sharedFiles = listOf(
                    SharedFileRef(
                        url = "https://files/1",
                        name = "one.png",
                        mediaType = "image/png",
                        sizeBytes = 42,
                        disposition = FileDisposition.INLINE,
                    ),
                    SharedFileRef(
                        url = "https://files/2",
                        disposition = FileDisposition.ATTACHMENT,
                    ),
                ),
                mentions = listOf(
                    MentionRef("xmpp:alice@waddle.test", 0u, 5u),
                    MentionRef("xmpp:bob@waddle.test", 6u, 9u),
                ),
            ),
        )

        assertEquals(
            DeliveryPayloadDigest(
                "v1:sha256:960a32674200872efad87e49366049bd111be01cbc67d587cb9d198eb5570223",
            ),
            QueuedOutboundMessage.computeStructuralDigest(
                plainPayload(body = "he\u0301llo"),
            ),
        )
        assertEquals(
            DeliveryPayloadDigest(
                "v1:sha256:e1b0b32621a87e63bdb2d39cf802d51bdefea74372294096c6e5357a351604d2",
            ),
            QueuedOutboundMessage.computeStructuralDigest(annotated),
        )
    }

    @Test
    fun `source is excluded from structural digest`() {
        val payload = plainPayload()
        val composer = draft(payload, DeliverySource.Composer)
        val directReply = draft(
            payload,
            DeliverySource.DirectReply(
                conversationJid = PEER,
                isGroupchat = false,
            ),
        )

        assertEquals(composer.payloadDigest, directReply.payloadDigest)
    }

    private fun draft(
        payload: QueuedOutboundPayload,
        source: DeliverySource,
    ): QueuedOutboundDraft = QueuedOutboundDraft.create(
        ownerBareJid = OWNER,
        clientStanzaId = "origin-1",
        enqueuedAtMillis = 1_000,
        payload = payload,
        source = source,
        incarnation = DeliveryIncarnation("00000000-0000-4000-8000-000000000001"),
    )

    private fun plainPayload(body: String = "hello"): QueuedOutboundPayload =
        QueuedOutboundPayload(
            target = QueuedOutboundTarget.Chat(PEER),
            content = QueuedOutboundContent(body),
        )

    private companion object {
        val JSON = Json
        const val OWNER = "alice@waddle.test"
        const val PEER = "peer@waddle.test"
    }
}
