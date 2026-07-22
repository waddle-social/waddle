package social.waddle.android.feature.profile

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
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
import social.waddle.android.client.FakeClientFactory
import social.waddle.android.client.FakeNetworkSignal
import social.waddle.android.client.InMemoryPreferencesDataStore
import social.waddle.android.client.PinnedRandom
import social.waddle.android.client.ReconnectPolicy
import social.waddle.android.client.RecordedProfileVerb
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.auth.WaddleSessionInfo
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

        val currentSession = MutableStateFlow<WaddleSessionInfo?>(
            testSessionInfo(jid = "icepuma@waddle.test"),
        )

        fun viewModel() = ProfileViewModel(
            sessionManager = manager,
            currentSession = currentSession,
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
    fun `tune publish is immediate and surfaces the wire result`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        // Manual submit: publishes right away (web parity), no
        // debounce window, no SCHEDULED intermediate state.
        viewModel.setTuneField(ProfileViewModel.TuneField.TITLE, "Come Together")
        viewModel.publishTune()
        runCurrent()

        val published = harness.client.profileVerbs
            .filterIsInstance<RecordedProfileVerb.PublishTune>()
            .single()
        assertEquals("Come Together", published.tune.title)
        assertEquals(ProfileFeedback.PUBLISHED, viewModel.uiState.value.tune.feedback)
        assertFalse(viewModel.uiState.value.tune.busy)
        harness.manager.logout()
    }

    @Test
    fun `a refused tune publish reports FAILED immediately`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()
        harness.client.profileVerbFailure = RuntimeException("not-acceptable")

        // Re-publishing the SAME tune value twice would leave the
        // selfTune StateFlow unchanged (conflation) — feedback must
        // come from the returned result, never from awaiting an
        // emission that cannot happen.
        viewModel.setTuneField(ProfileViewModel.TuneField.TITLE, "Nope")
        viewModel.publishTune()
        runCurrent()

        assertEquals(ProfileFeedback.FAILED, viewModel.uiState.value.tune.feedback)
        assertFalse(viewModel.uiState.value.tune.busy)
        harness.manager.logout()
    }

    @Test
    fun `clearTune retracts`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        viewModel.clearTune()
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

        viewModel.publishAvatar { null }
        runCurrent()
        assertEquals(ProfileFeedback.FAILED, viewModel.uiState.value.avatar.feedback)
        assertTrue(harness.client.profileVerbs.isEmpty())

        viewModel.publishAvatar {
            ProcessedAvatar(data = byteArrayOf(1, 2, 3), mimeType = "image/png", width = 64, height = 64)
        }
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
    fun `avatar busy flips before processing and blocks a second pick`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        val viewModel = harness.viewModel()
        runCurrent()

        // Park the pipeline mid-processing: busy must already be set.
        val processing = CompletableDeferred<ProcessedAvatar?>()
        viewModel.publishAvatar { processing.await() }
        runCurrent()
        assertTrue(viewModel.uiState.value.avatar.busy)

        // A second pick while one is in flight is refused outright.
        var secondRan = false
        viewModel.publishAvatar {
            secondRan = true
            null
        }
        runCurrent()
        assertFalse(secondRan)

        processing.complete(
            ProcessedAvatar(data = byteArrayOf(9), mimeType = "image/png", width = 8, height = 8),
        )
        runCurrent()
        assertFalse(viewModel.uiState.value.avatar.busy)
        assertEquals(
            RecordedProfileVerb.PublishAvatar(1, "image/png", 8u, 8u),
            harness.client.profileVerbs.single(),
        )
        harness.manager.logout()
    }

    @Test
    fun `a failed load exposes retry which reloads`() = runTest {
        val harness = Harness(this)
        harness.loginReady(this)
        harness.client.fetchVcard4Failure = RuntimeException("boom")
        val viewModel = harness.viewModel()
        runCurrent()

        assertTrue(viewModel.uiState.value.vcard.loadFailed)
        assertFalse(viewModel.uiState.value.vcard.loaded)

        // The transient failure clears; Retry reloads without waiting
        // for a reconnect.
        harness.client.fetchVcard4Failure = null
        harness.client.vcard4 = testVcard4(fullName = "Ice Puma")
        viewModel.retryLoad()
        runCurrent()

        val state = viewModel.uiState.value.vcard
        assertTrue(state.loaded)
        assertFalse(state.loadFailed)
        assertEquals("Ice Puma", state.fullName)
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

        // Identity is a flow: when the session flips to another
        // account, the avatar follows the CURRENT session.
        harness.currentSession.value = testSessionInfo(jid = "pingu@waddle.test")
        runCurrent()
        assertNull(viewModel.selfAvatar.value)
        harness.manager.logout()
    }
}
