package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.MentionCandidate
import social.waddle.android.client.MentionRef

class MentionSpanTrackerTest {
    private val bob = MentionCandidate(
        display = "bob",
        uri = "xmpp:bob@waddle.test",
        isBroadcast = false,
    )

    private fun tracker(initial: String): MentionSpanTracker =
        MentionSpanTracker().apply { onTextChanged(initial) }

    @Test
    fun `inserting a candidate replaces the token and records the ref`() {
        val tracker = tracker("hi @bo")

        val insertion = tracker.insertMention("hi @bo", MentionToken(3, 6, "bo"), bob)

        assertEquals("hi @bob ", insertion?.text)
        assertEquals(8, insertion?.cursor)
        assertEquals(
            listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 3u, end = 7u)),
            tracker.mentionRefs(),
        )
    }

    @Test
    fun `typing before the label shifts the recorded span`() {
        val tracker = tracker("@bo")
        tracker.insertMention("@bo", MentionToken(0, 3, "bo"), bob)

        tracker.onTextChanged("well @bob ")

        assertEquals(
            listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 5u, end = 9u)),
            tracker.mentionRefs(),
        )
    }

    @Test
    fun `editing inside the label drops the mention`() {
        val tracker = tracker("@bo")
        tracker.insertMention("@bo", MentionToken(0, 3, "bo"), bob)

        tracker.onTextChanged("@bXob ")

        assertTrue(tracker.mentionRefs().isEmpty())
    }

    @Test
    fun `offsets are codepoints over the trimmed body`() {
        // Leading whitespace is trimmed off the wire body and the emoji
        // is one code point (two UTF-16 units) — begin/end must count
        // code points over exactly what gets sent.
        val tracker = tracker("  😀 @bo")

        tracker.insertMention("  😀 @bo", MentionToken(5, 8, "bo"), bob)

        assertEquals(
            listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 2u, end = 6u)),
            tracker.mentionRefs(),
        )
    }

    @Test
    fun `a stale span whose text no longer reads the label is dropped`() {
        val tracker = tracker("@bo")
        tracker.insertMention("@bo", MentionToken(0, 3, "bo"), bob)

        // A replacement overlapping the label must never survive into a
        // wire reference pointing at unrelated text.
        tracker.onTextChanged("@rob still")

        assertTrue(tracker.mentionRefs().isEmpty())
    }

    @Test
    fun `clearing the draft forgets every span`() {
        val tracker = tracker("@bo")
        tracker.insertMention("@bo", MentionToken(0, 3, "bo"), bob)

        tracker.onTextChanged("")

        assertTrue(tracker.mentionRefs().isEmpty())
    }

    @Test
    fun `a stale token outside the text is rejected`() {
        val tracker = tracker("hi")

        assertNull(tracker.insertMention("hi", MentionToken(1, 9, "x"), bob))
    }
}
