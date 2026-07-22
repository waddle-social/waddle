package social.waddle.android.feature.settings

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testSessionInfo

@OptIn(ExperimentalCoroutinesApi::class)
class SettingsViewModelTest {
    private val userPrefs = UserPrefs(InMemoryPreferencesDataStore())
    private var signOutCalls = 0
    private val currentSession = MutableStateFlow<WaddleSessionInfo?>(null)

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun createViewModel(withSession: Boolean = true): SettingsViewModel {
        currentSession.value = if (withSession) testSessionInfo() else null
        return SettingsViewModel(
            userPrefs = userPrefs,
            currentSession = currentSession,
            performSignOut = { signOutCalls += 1 },
        )
    }

    @Test
    fun `account row reflects the signed-in session`() = runTest {
        val viewModel = createViewModel()
        runCurrent()

        val state = viewModel.uiState.value
        assertEquals("icepuma", state.username)
        assertEquals("icepuma@waddle.test", state.jid)
        assertNull(state.avatarUrl)
    }

    @Test
    fun `account row follows the CURRENT session across a relogin`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        assertEquals("icepuma@waddle.test", viewModel.uiState.value.jid)

        // Logout → login as another account: the row must track the
        // live session flow, not a constructor-frozen snapshot.
        currentSession.value = null
        runCurrent()
        assertNull(viewModel.uiState.value.jid)

        currentSession.value = testSessionInfo(username = "pingu", jid = "pingu@waddle.test")
        runCurrent()
        assertEquals("pingu", viewModel.uiState.value.username)
        assertEquals("pingu@waddle.test", viewModel.uiState.value.jid)
    }

    @Test
    fun `defaults are system theme with everything enabled`() = runTest {
        val viewModel = createViewModel(withSession = false)
        runCurrent()

        assertEquals(
            SettingsUiState(
                theme = ThemeMode.SYSTEM,
                notificationsEnabled = true,
                messageSoundsEnabled = true,
                readReceiptsEnabled = true,
            ),
            viewModel.uiState.value,
        )
    }

    @Test
    fun `theme round-trips through the prefs store`() = runTest {
        val viewModel = createViewModel()
        viewModel.setTheme(ThemeMode.DARK)
        runCurrent()

        assertEquals(ThemeMode.DARK, viewModel.uiState.value.theme)
        assertEquals(ThemeMode.DARK, userPrefs.theme.first())
    }

    @Test
    fun `notification prefs round-trip through the prefs store`() = runTest {
        val viewModel = createViewModel()

        viewModel.setNotificationsEnabled(false)
        viewModel.setMessageSoundsEnabled(false)
        viewModel.setReadReceiptsEnabled(false)
        runCurrent()

        val state = viewModel.uiState.value
        assertFalse(state.notificationsEnabled)
        assertFalse(state.messageSoundsEnabled)
        assertFalse(state.readReceiptsEnabled)
        assertFalse(userPrefs.notificationsEnabled.first())
        assertFalse(userPrefs.messageSoundsEnabled.first())
        assertFalse(userPrefs.readReceiptsEnabled.first())

        viewModel.setNotificationsEnabled(true)
        runCurrent()
        assertTrue(viewModel.uiState.value.notificationsEnabled)
        assertTrue(userPrefs.notificationsEnabled.first())
    }

    @Test
    fun `pref toggles keep the account row intact`() = runTest {
        val viewModel = createViewModel()
        viewModel.setTheme(ThemeMode.LIGHT)
        runCurrent()

        assertEquals("icepuma", viewModel.uiState.value.username)
        assertEquals("icepuma@waddle.test", viewModel.uiState.value.jid)
    }

    @Test
    fun `logout invokes the sign-out callback once`() = runTest {
        val viewModel = createViewModel()
        viewModel.logout()
        runCurrent()

        assertEquals(1, signOutCalls)
    }
}
