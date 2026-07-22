package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Pins the closed XEP-0107/0108 vocabularies mirrored from the web
 *  client (`pep-types.ts`) and the server's typed enums. */
class PepVocabularyTest {
    private val snakeCase = Regex("^[a-z0-9]+(_[a-z0-9]+)*$")

    @Test
    fun `the mood vocabulary is the 84 defined kinds, unique and snake_case`() {
        assertEquals(84, MOOD_KINDS.size)
        assertEquals(MOOD_KINDS.size, MOOD_KINDS.toSet().size)
        assertTrue(MOOD_KINDS.all { snakeCase.matches(it) })
        assertTrue("happy" in MOOD_KINDS)
        assertTrue("undefined" in MOOD_KINDS)
    }

    @Test
    fun `the activity vocabulary is the 12 defined categories`() {
        assertEquals(12, GENERAL_ACTIVITIES.size)
        assertEquals(GENERAL_ACTIVITIES.size, GENERAL_ACTIVITIES.toSet().size)
        assertTrue(GENERAL_ACTIVITIES.all { snakeCase.matches(it) })
    }

    @Test
    fun `the on-call specifics ride the talking category`() {
        assertTrue(ACTIVITY_GENERAL_TALKING in GENERAL_ACTIVITIES)
        assertTrue(snakeCase.matches(ACTIVITY_SPECIFIC_ON_THE_PHONE))
        assertTrue(snakeCase.matches(ACTIVITY_SPECIFIC_ON_VIDEO_PHONE))
    }
}
