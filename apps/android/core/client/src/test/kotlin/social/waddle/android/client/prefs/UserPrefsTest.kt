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
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class UserPrefsTest {
    @get:Rule
    val tempFolder = TemporaryFolder()

    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    private fun newPrefs(): UserPrefs {
        val file = File(tempFolder.root, "user.preferences_pb")
        return UserPrefs(PreferenceDataStoreFactory.create(scope = scope) { file })
    }

    @After
    fun tearDown() {
        scope.cancel()
    }

    @Test
    fun `defaults are system theme with notifications and sounds on`() = runBlocking {
        val prefs = newPrefs()

        assertEquals(ThemeMode.SYSTEM, prefs.theme.first())
        assertTrue(prefs.notificationsEnabled.first())
        assertTrue(prefs.messageSoundsEnabled.first())
    }

    @Test
    fun `round trips settings and clear restores defaults`() = runBlocking {
        val prefs = newPrefs()

        prefs.setTheme(ThemeMode.DARK)
        prefs.setNotificationsEnabled(false)
        prefs.setMessageSoundsEnabled(false)

        assertEquals(ThemeMode.DARK, prefs.theme.first())
        assertFalse(prefs.notificationsEnabled.first())
        assertFalse(prefs.messageSoundsEnabled.first())

        prefs.clear()
        assertEquals(ThemeMode.SYSTEM, prefs.theme.first())
        assertTrue(prefs.notificationsEnabled.first())
    }
}
