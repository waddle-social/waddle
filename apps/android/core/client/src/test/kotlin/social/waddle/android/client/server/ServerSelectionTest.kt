package social.waddle.android.client.server

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import social.waddle.android.client.prefs.UserPrefs
import java.io.File

class ServerSelectionTest {
    @get:Rule
    val tempFolder = TemporaryFolder()

    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private val prefs by lazy {
        UserPrefs(PreferenceDataStoreFactory.create(scope = scope) { File(tempFolder.root, "user.preferences_pb") })
    }

    @After
    fun tearDown() {
        scope.cancel()
    }

    private fun selection() = ServerSelection(prefs, "https://waddle.default")

    @Test
    fun `starts on the default`() = runTest {
        val selection = selection()
        selection.load()
        assertEquals("https://waddle.default", selection.current.value)
    }

    @Test
    fun `a selected server survives a restart`() = runTest {
        selection().select("https://chat.example")

        val restarted = selection()
        restarted.load()

        assertEquals("https://chat.example", restarted.current.value)
    }

    @Test
    fun `selecting the default clears the override`() = runTest {
        val selection = selection()
        selection.select("https://chat.example")
        selection.reset()

        assertNull(prefs.serverUrl.first())
        assertEquals("https://waddle.default", selection.current.value)
    }

    @Test
    fun `a stored session origin wins over the preference`() = runTest {
        selection().select("https://chat.example")

        val restarted = selection()
        restarted.load(sessionOrigin = "https://other.example")

        assertEquals("https://other.example", restarted.current.value)
    }
}
