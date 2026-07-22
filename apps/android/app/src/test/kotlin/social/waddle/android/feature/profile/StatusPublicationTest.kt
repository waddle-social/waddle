package social.waddle.android.feature.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Web `status-publication-ui.test` parity for the ported builders. */
class StatusPublicationTest {
    @Test
    fun `formatPepKeyword renders snake_case as words`() {
        assertEquals("In Awe", formatPepKeyword("in_awe"))
        assertEquals("Happy", formatPepKeyword("happy"))
        assertEquals("Doing Chores", formatPepKeyword("doing_chores"))
    }

    @Test
    fun `normalizeActivitySpecific collapses free text to snake_case`() {
        assertEquals("deep_focus", normalizeActivitySpecific("  Deep Focus "))
        assertEquals("rockin_out", normalizeActivitySpecific("Rockin' Out!"))
        assertEquals("", normalizeActivitySpecific("!!!"))
    }

    @Test
    fun `mood requires a kind and trims the note`() {
        assertNull(buildMoodPublication(MoodDraft(kind = "", text = "hello")))
        val mood = buildMoodPublication(MoodDraft(kind = "happy", text = "  hi  "))
        assertEquals("happy", mood?.kind)
        assertEquals("hi", mood?.text)
        assertNull(buildMoodPublication(MoodDraft(kind = "happy", text = "   "))?.text)
    }

    @Test
    fun `activity requires a general category and a clean specific`() {
        val missing = buildActivityPublication(ActivityDraft(general = "", specific = "", text = ""))
        assertNull(missing.publication)
        assertEquals(setOf(ActivityFieldError.MISSING_GENERAL), missing.errors)

        val badSpecific = buildActivityPublication(ActivityDraft(general = "working", specific = "!!!", text = ""))
        assertNull(badSpecific.publication)
        assertEquals(setOf(ActivityFieldError.INVALID_SPECIFIC), badSpecific.errors)

        val ok = buildActivityPublication(ActivityDraft(general = "working", specific = "Deep Focus", text = " hi "))
        assertEquals("working", ok.publication?.general)
        assertEquals("deep_focus", ok.publication?.specific)
        assertEquals("hi", ok.publication?.text)
        assertTrue(ok.errors.isEmpty())
    }

    @Test
    fun `an empty specific stays optional`() {
        val ok = buildActivityPublication(ActivityDraft(general = "relaxing", specific = "  ", text = ""))
        assertNull(ok.publication?.specific)
        assertTrue(ok.errors.isEmpty())
    }

    @Test
    fun `tune needs at least one detail`() {
        val empty = buildTunePublication(TuneDraft())
        assertNull(empty.publication)
        assertEquals(setOf(TuneFieldError.EMPTY_FORM), empty.errors)
    }

    @Test
    fun `tune length and rating validate as bounded integers`() {
        val bad = buildTunePublication(TuneDraft(title = "x", length = "abc", rating = "11"))
        assertNull(bad.publication)
        assertEquals(setOf(TuneFieldError.INVALID_LENGTH, TuneFieldError.INVALID_RATING), bad.errors)

        val zeroLength = buildTunePublication(TuneDraft(title = "x", length = "0"))
        assertEquals(setOf(TuneFieldError.INVALID_LENGTH), zeroLength.errors)

        val ok = buildTunePublication(TuneDraft(title = "x", length = "259", rating = "9"))
        assertEquals(259u, ok.publication?.lengthSeconds)
        assertEquals(9u.toUByte(), ok.publication?.rating)
    }

    @Test
    fun `tune uri must be absolute but accepts non-http schemes`() {
        val bad = buildTunePublication(TuneDraft(uri = "not a uri"))
        assertTrue(TuneFieldError.INVALID_URI in bad.errors)

        val relative = buildTunePublication(TuneDraft(uri = "path/to/track"))
        assertTrue(TuneFieldError.INVALID_URI in relative.errors)

        val https = buildTunePublication(TuneDraft(uri = "https://example.com/track"))
        assertEquals("https://example.com/track", https.publication?.uri)

        val spotify = buildTunePublication(TuneDraft(uri = "spotify:track:abc123"))
        assertEquals("spotify:track:abc123", spotify.publication?.uri)

        // Web-effective parity: any parseable absolute URI passes,
        // single-letter schemes included (the web's protocol-length
        // check is vacuous — `URL.protocol` carries the trailing `:`).
        val singleLetter = buildTunePublication(TuneDraft(uri = "x:track"))
        assertEquals("x:track", singleLetter.publication?.uri)
    }

    @Test
    fun `tune fields trim and blank out to null`() {
        val ok = buildTunePublication(TuneDraft(artist = "  The Beatles ", title = "  ", source = "Abbey Road"))
        assertEquals("The Beatles", ok.publication?.artist)
        assertNull(ok.publication?.title)
        assertEquals("Abbey Road", ok.publication?.source)
    }
}
