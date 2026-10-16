package social.waddle.android.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The decrypted v1 payload (`waddle-xmpp/src/push/envelope.rs`). */
class PushEnvelopeTest {
    private fun parse(json: String) = PushEnvelope.parse(json.encodeToByteArray())

    @Test
    fun `parses a dm envelope`() {
        val envelope = parse("""{"v":1,"class":"dm","conversation":"alice@waddle.test","item":"s-1","unread":3}""")!!
        assertEquals(PushClass.DM, envelope.pushClass)
        assertEquals("alice@waddle.test", envelope.conversation)
        assertEquals("s-1", envelope.item)
        assertEquals(3L, envelope.unread)
        assertFalse(envelope.isGroupchat)
        assertFalse(envelope.isMention)
    }

    @Test
    fun `room classes are groupchat and mentions are mentions`() {
        val mention = parse(
            """{"v":1,"class":"channel_mention","conversation":"general@muc.waddle.test","thread":"t","item":"s"}""",
        )!!
        assertTrue(mention.isGroupchat)
        assertTrue(mention.isMention)
        assertEquals("t", mention.thread)
        val all = parse("""{"v":1,"class":"notify_all","conversation":"general@muc.waddle.test","item":"s"}""")!!
        assertTrue(all.isGroupchat)
        assertFalse(all.isMention)
        val dmMention = parse("""{"v":1,"class":"dm_mention","conversation":"a@waddle.test","item":"s"}""")!!
        assertFalse(dmMention.isGroupchat)
        assertTrue(dmMention.isMention)
    }

    @Test
    fun `trailing padding does not break the parse`() {
        val padded = """{"v":1,"class":"dm","conversation":"a@waddle.test","item":"s"}""".encodeToByteArray() +
            ByteArray(16)
        assertEquals("a@waddle.test", PushEnvelope.parse(padded)?.conversation)
    }

    @Test
    fun `rejects anything that is not a v1 envelope naming a bare jid`() {
        listOf(
            "not json",
            """{"v":2,"class":"dm","conversation":"a@waddle.test","item":"s"}""",
            """{"v":1,"class":"call","conversation":"a@waddle.test","item":"s"}""",
            """{"v":1,"class":"dm","conversation":"a@waddle.test/phone","item":"s"}""",
            """{"v":1,"class":"dm","conversation":"a b@waddle.test","item":"s"}""",
            """{"v":1,"class":"dm","conversation":"@waddle.test","item":"s"}""",
            """{"v":1,"class":"dm","conversation":"a@waddle.test","item":""}""",
            """{"v":1,"class":"dm","item":"s"}""",
        ).forEach { assertNull(it, parse(it)) }
    }
}
