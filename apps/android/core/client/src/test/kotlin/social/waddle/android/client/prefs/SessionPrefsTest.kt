package social.waddle.android.client.prefs

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** Plain-JVM DataStore over a temp dir — no Robolectric needed. */
class SessionPrefsTest {
    @get:Rule
    val tempFolder = TemporaryFolder()

    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    private fun newPrefs(): SessionPrefs {
        val file = File(tempFolder.root, "session.preferences_pb")
        return SessionPrefs(PreferenceDataStoreFactory.create(scope = scope) { file })
    }

    @After
    fun tearDown() {
        scope.cancel()
    }

    @Test
    fun `round trips scalar session fields`() = runBlocking {
        val prefs = newPrefs()

        prefs.setServerUrl("https://waddle.test")
        prefs.setSessionId("sess-1")
        prefs.setJoinedRooms(setOf("general@muc.waddle.test"))
        prefs.setLastSeen("alice@waddle.test", "2026-07-15T10:00:00Z")

        assertEquals("https://waddle.test", prefs.serverUrl.first())
        assertEquals("sess-1", prefs.sessionId.first())
        assertEquals(setOf("general@muc.waddle.test"), prefs.joinedRooms.first())
        assertEquals(mapOf("alice@waddle.test" to "2026-07-15T10:00:00Z"), prefs.lastSeen.first())
    }

    @Test
    fun `round trips the resume snapshot and null clears it`() = runBlocking {
        val prefs = newPrefs()
        val snapshot = SmResumeSnapshot(
            previd = "prev-1",
            inboundH = 5u,
            outboundH = 7u,
            maxResumeSeconds = 300u,
            queuedStanzasXml = listOf("<message/>"),
        )

        prefs.setSmResume(snapshot)
        assertEquals(snapshot, prefs.smResume.first())

        prefs.setSmResume(null)
        assertNull(prefs.smResume.first())
    }

    @Test
    fun `resource suffix is eight hex chars and stable across clear`() = runBlocking {
        val prefs = newPrefs()

        val suffix = prefs.resourceSuffix()
        assertTrue("expected 8 hex chars, got $suffix", suffix.matches(Regex("[0-9a-f]{8}")))
        assertEquals(suffix, prefs.resourceSuffix())

        prefs.setSessionId("sess-1")
        prefs.clear()

        assertNull(prefs.sessionId.first())
        assertEquals("per-install suffix survives logout", suffix, prefs.resourceSuffix())
    }
}
