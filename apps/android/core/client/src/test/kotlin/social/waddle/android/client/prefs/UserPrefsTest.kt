package social.waddle.android.client.prefs

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
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
import java.io.File

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
    fun `defaults are system theme, chat timeline, notifications and sounds on`() = runBlocking {
        val prefs = newPrefs()

        assertEquals(ThemeMode.SYSTEM, prefs.theme.first())
        assertEquals(TimelineDirection.CHAT, prefs.timelineDirection.first())
        assertTrue(prefs.notificationsEnabled.first())
        assertTrue(prefs.messageSoundsEnabled.first())
    }

    @Test
    fun `round trips settings and clear restores defaults`() = runBlocking {
        val prefs = newPrefs()

        prefs.setTheme(ThemeMode.DAYLIGHT)
        prefs.setTimelineDirection(TimelineDirection.SOCIAL)
        prefs.setNotificationsEnabled(false)
        prefs.setMessageSoundsEnabled(false)

        assertEquals(ThemeMode.DAYLIGHT, prefs.theme.first())
        assertEquals(TimelineDirection.SOCIAL, prefs.timelineDirection.first())
        assertFalse(prefs.notificationsEnabled.first())
        assertFalse(prefs.messageSoundsEnabled.first())

        prefs.clear()
        assertEquals(ThemeMode.SYSTEM, prefs.theme.first())
        assertEquals(TimelineDirection.CHAT, prefs.timelineDirection.first())
        assertTrue(prefs.notificationsEnabled.first())
    }

    @Test
    fun `nothing stored and unknown values follow the system`() {
        assertEquals(ThemeMode.SYSTEM, ThemeMode.fromStorage("system"))
        assertEquals(ThemeMode.SYSTEM, ThemeMode.fromStorage(null))
        assertEquals(ThemeMode.SYSTEM, ThemeMode.fromStorage("sepia"))
        assertEquals(ThemeMode.NIGHT, ThemeMode.fromStorage("dark"))
        assertEquals(ThemeMode.DAYLIGHT, ThemeMode.fromStorage("light"))
    }

    @Test
    fun `system follows the phone, the pinned themes do not`() {
        assertTrue(ThemeMode.SYSTEM.isNight(systemDark = true))
        assertFalse(ThemeMode.SYSTEM.isNight(systemDark = false))
        assertTrue(ThemeMode.NIGHT.isNight(systemDark = false))
        assertFalse(ThemeMode.DAYLIGHT.isNight(systemDark = true))
    }

    @Test
    fun `unknown timeline directions read as chat`() {
        assertEquals(TimelineDirection.CHAT, TimelineDirection.fromStorage(null))
        assertEquals(TimelineDirection.CHAT, TimelineDirection.fromStorage("sideways"))
        assertEquals(TimelineDirection.SOCIAL, TimelineDirection.fromStorage("social"))
    }

    @Test
    fun `wallpaper colours are off until chosen`() = runBlocking {
        val prefs = newPrefs()
        assertFalse(prefs.wallpaperColors.first())

        prefs.setWallpaperColors(true)

        assertTrue(prefs.wallpaperColors.first())
    }
}
