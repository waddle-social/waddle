package social.waddle.android.feature.profile

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.RecordedProfileVerb
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.testAvatar
import social.waddle.android.client.testSessionInfo
import social.waddle.android.client.testVcard4
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleMood
import social.waddle.client.ffi.WaddlePepProfile

@OptIn(ExperimentalCoroutinesApi::class)
class ProfileViewModelTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )

        suspend fun loginReady(scope: TestScope) {
            manager.login(testSessionInfo())
            scope.runCurrent()
            factory.emit(WaddleClientEvent.Connected)
            scope.runCurrent()
        }

        val client get() = factory.clients.last()

        fun viewModel() = ProfileViewModel(
            sessionManager = manager,
            ownJid = "icepuma@waddle.test",
            restAvatarUrl = "https://rest.example/avatar.png",
        )
    }

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `load seeds the vcard and status drafts from the server`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.vcard4 = testVcard4(fullName = "Ice Puma", nickname = "ice", note = "hello")
        harness.client.pepProfile = WaddlePepProfile(
            mood = WaddleMood(kind = "happy", text = "great"),
            activity = null,
            tune = null,
        )

        val viewModel = harness.viewModel()
        runCurrent()

        val state = viewModel.uiState.value
        assertTrue(state.vcard.loaded)
        assertEquals("Ice Puma", state.vcard.fullName)
        assertEquals("ice", state.vcard.nickname)
        assertEquals("hello", state.vcard.note)
        assertEquals("happy", state.mood.draft.kind)
        assertEquals("great", state.mood.draft.text)
        harness.manager.logout()
    }

    @Test
    fun `saveVcard publishes optimistically and reports success`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.setFullName("New Name")
        viewModel.saveVcard()
        runCurrent()

        val published = harness.client.profileVerbs
            .filterIsInstance<RecordedProfileVerb.PublishVcard4>()
            .single()
        assertEquals("New Name", published.vcard.fullName)
        assertEquals(ProfileFeedback.PUBLISHED, viewModel.uiState.value.vcard.feedback)
        assertEquals("New Name", harness.manager.profileStore.selfVcard.value?.fullName)
        harness.manager.logout()
    }

    @Test
    fun `a refused save rolls the draft back to the last persisted profile`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.vcard4 = testVcard4(fullName = "Old Name")
        val viewModel = harness.viewModel()
        runCurrent()
        harness.client.profileVerbFailure = RuntimeException("rejected")

        viewModel.setFullName("New Name")
        viewModel.saveVcard()
        runCurrent()

        val state = viewModel.uiState.value.vcard
        assertEquals("Old Name", state.fullName)
        assertEquals(ProfileFeedback.FAILED, state.feedback)
        assertEquals("Old Name", harness.manager.profileStore.selfVcard.value?.fullName)
        harness.manager.logout()
    }

    @Test
    fun `an unchanged draft publishes nothing`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.vcard4 = testVcard4(fullName = "Same")
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.saveVcard()
        runCurrent()

        assertTrue(harness.client.profileVerbs.isEmpty())
        assertEquals(ProfileFeedback.UNCHANGED, viewModel.uiState.value.vcard.feedback)
        harness.manager.logout()
    }

    @Test
    fun `mood publish validates the kind and records the verb`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.publishMood()
        assertEquals(ProfileFeedback.INVALID, viewModel.uiState.value.mood.feedback)
        assertTrue(harness.client.profileVerbs.isEmpty())

        viewModel.setMoodKind("happy")
        viewModel.setMoodText("  hi  ")
        viewModel.publishMood()
        runCurrent()

        assertEquals(
            listOf<RecordedProfileVerb>(RecordedProfileVerb.PublishMood("happy", "hi")),
            harness.client.profileVerbs,
        )
        assertEquals(ProfileFeedback.PUBLISHED, viewModel.uiState.value.mood.feedback)

        viewModel.clearMood()
        runCurrent()
        assertEquals(RecordedProfileVerb.RetractMood, harness.client.profileVerbs.last())
        assertEquals(ProfileFeedback.CLEARED, viewModel.uiState.value.mood.feedback)
        assertEquals("", viewModel.uiState.value.mood.draft.kind)
        harness.manager.logout()
    }

    @Test
    fun `activity publish normalizes the specific and records the verb`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.setActivityGeneral("working")
        viewModel.setActivitySpecific("Deep Focus")
        viewModel.publishActivity()
        runCurrent()

        assertEquals(
            listOf<RecordedProfileVerb>(RecordedProfileVerb.PublishActivity("working", "deep_focus", null)),
            harness.client.profileVerbs,
        )
        assertEquals("deep_focus", viewModel.uiState.value.activity.draft.specific)

        viewModel.clearActivity()
        runCurrent()
        assertEquals(RecordedProfileVerb.RetractActivity, harness.client.profileVerbs.last())
        assertEquals("", viewModel.uiState.value.activity.draft.general)
        harness.manager.logout()
    }

    @Test
    fun `tune publish is debounced and confirms once the store reflects it`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.setTuneField(ProfileViewModel.TuneField.TITLE, "Come Together")
        viewModel.publishTune()
        runCurrent()

        assertEquals(ProfileFeedback.SCHEDULED, viewModel.uiState.value.tune.feedback)
        assertTrue(harness.client.profileVerbs.isEmpty())

        advanceTimeBy(XmppSessionManager.TUNE_DEBOUNCE_MILLIS + 1)
        runCurrent()

        val published = harness.client.profileVerbs
            .filterIsInstance<RecordedProfileVerb.PublishTune>()
            .single()
        assertEquals("Come Together", published.tune.title)
        assertEquals(ProfileFeedback.PUBLISHED, viewModel.uiState.value.tune.feedback)
        harness.manager.logout()
    }

    @Test
    fun `clearTune cancels a pending debounced publish`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.setTuneField(ProfileViewModel.TuneField.TITLE, "Skipped")
        viewModel.publishTune()
        runCurrent()
        viewModel.clearTune()
        advanceTimeBy(XmppSessionManager.TUNE_DEBOUNCE_MILLIS + 1)
        runCurrent()

        assertEquals(
            listOf<RecordedProfileVerb>(RecordedProfileVerb.RetractTune),
            harness.client.profileVerbs,
        )
        assertEquals(ProfileFeedback.CLEARED, viewModel.uiState.value.tune.feedback)
        harness.manager.logout()
    }

    @Test
    fun `an invalid tune never reaches the manager`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.setTuneField(ProfileViewModel.TuneField.RATING, "11")
        viewModel.publishTune()
        advanceTimeBy(XmppSessionManager.TUNE_DEBOUNCE_MILLIS + 1)
        runCurrent()

        assertTrue(harness.client.profileVerbs.isEmpty())
        assertEquals(ProfileFeedback.INVALID, viewModel.uiState.value.tune.feedback)
        assertTrue(TuneFieldError.INVALID_RATING in viewModel.uiState.value.tune.errors)
        harness.manager.logout()
    }

    @Test
    fun `avatar publish and remove record their verbs`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.publishAvatar(null)
        assertEquals(ProfileFeedback.FAILED, viewModel.uiState.value.avatar.feedback)
        assertTrue(harness.client.profileVerbs.isEmpty())

        viewModel.publishAvatar(
            ProcessedAvatar(data = byteArrayOf(1, 2, 3), mimeType = "image/png", width = 64, height = 64),
        )
        runCurrent()
        assertEquals(
            RecordedProfileVerb.PublishAvatar(3, "image/png", 64u, 64u),
            harness.client.profileVerbs.single(),
        )
        assertEquals(ProfileFeedback.PUBLISHED, viewModel.uiState.value.avatar.feedback)

        viewModel.removeAvatar()
        runCurrent()
        assertEquals(RecordedProfileVerb.DisableAvatar, harness.client.profileVerbs.last())
        harness.manager.logout()
    }

    @Test
    fun `selfAvatar surfaces the account's XMPP avatar from the store`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()
        assertNull(viewModel.selfAvatar.value)

        harness.manager.profileStore.onAvatar(testAvatar(jid = "icepuma@waddle.test", id = "id-1"))
        runCurrent()

        assertEquals("id-1", viewModel.selfAvatar.value?.id)
        harness.manager.logout()
    }
}
