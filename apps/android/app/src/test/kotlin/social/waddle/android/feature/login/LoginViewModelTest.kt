package social.waddle.android.feature.login

import java.net.SocketTimeoutException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.auth.AuthError
import social.waddle.android.client.auth.AuthProvider
import social.waddle.android.client.auth.DevicePollResult
import social.waddle.android.client.auth.DeviceStartResponse
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.testSessionInfo

@OptIn(ExperimentalCoroutinesApi::class)
class LoginViewModelTest {
    private class FakeLoginAuthGateway : LoginAuthGateway {
        var providersResult: Result<List<AuthProvider>> =
            Result.success(listOf(AuthProvider(id = "github", kind = "oauth", displayName = "GitHub")))
        var startResult: Result<DeviceStartResponse> = Result.success(
            DeviceStartResponse(
                deviceCode = "dev-1",
                userCode = "ABCD-1234",
                verificationUri = "https://waddle.test/verify",
                verificationUriComplete = "https://waddle.test/verify?code=ABCD-1234",
                interval = 5,
                expiresIn = 600,
            ),
        )
        val pollResults = ArrayDeque<Result<DevicePollResult>>()
        var sessionResult: Result<WaddleSessionInfo?> = Result.success(testSessionInfo())
        var pollCalls = 0

        override suspend fun providers(): Result<List<AuthProvider>> = providersResult

        override suspend fun startDeviceAuth(providerId: String): Result<DeviceStartResponse> =
            startResult

        override suspend fun pollDeviceAuth(deviceCode: String): Result<DevicePollResult> {
            pollCalls += 1
            return pollResults.removeFirstOrNull() ?: Result.success(DevicePollResult.Pending)
        }

        override suspend fun session(sessionId: String): Result<WaddleSessionInfo?> = sessionResult
    }

    private val gateway = FakeLoginAuthGateway()
    private val signedIn = mutableListOf<WaddleSessionInfo>()

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun createViewModel(): LoginViewModel =
        LoginViewModel(gateway = gateway) { signedIn += it }

    @Test
    fun `providers load into the picker`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        assertEquals(
            LoginUiState.ProviderPicker(
                listOf(AuthProvider(id = "github", kind = "oauth", displayName = "GitHub")),
            ),
            viewModel.uiState.value,
        )
    }

    @Test
    fun `provider load failure surfaces as error`() = runTest {
        gateway.providersResult = Result.failure(AuthError.Server(500, "boom"))
        val viewModel = createViewModel()
        runCurrent()
        val state = viewModel.uiState.value as LoginUiState.Failed
        assertEquals(LoginFailureReason.PROVIDERS_UNAVAILABLE, state.reason)
    }

    @Test
    fun `device flow polls to completion and signs in`() = runTest {
        gateway.pollResults += Result.success(DevicePollResult.Pending)
        gateway.pollResults += Result.success(DevicePollResult.Complete("sess-9"))
        val viewModel = createViewModel()
        runCurrent()

        viewModel.selectProvider("github")
        runCurrent()
        assertEquals(LoginUiState.Verifying("ABCD-1234"), viewModel.uiState.value)
        assertEquals(
            "https://waddle.test/verify?code=ABCD-1234",
            viewModel.openVerificationUrl.first(),
        )

        advanceTimeBy(5_000)
        runCurrent()
        assertEquals(1, gateway.pollCalls)
        assertEquals(LoginUiState.Verifying("ABCD-1234"), viewModel.uiState.value)

        advanceTimeBy(5_000)
        runCurrent()
        assertEquals(2, gateway.pollCalls)
        assertEquals(LoginUiState.Completing, viewModel.uiState.value)
        assertEquals(listOf(testSessionInfo()), signedIn)
    }

    @Test
    fun `transient poll errors retry on the next tick`() = runTest {
        gateway.pollResults += Result.failure(AuthError.Network(SocketTimeoutException("mid-flight")))
        gateway.pollResults += Result.success(DevicePollResult.Complete("sess-9"))
        val viewModel = createViewModel()
        runCurrent()
        viewModel.selectProvider("github")
        runCurrent()

        advanceTimeBy(10_000)
        runCurrent()
        assertEquals(2, gateway.pollCalls)
        assertEquals(1, signedIn.size)
    }

    @Test
    fun `fatal poll errors abort the flow`() = runTest {
        gateway.pollResults += Result.failure(AuthError.Server(400, "denied"))
        val viewModel = createViewModel()
        runCurrent()
        viewModel.selectProvider("github")
        runCurrent()

        advanceTimeBy(5_000)
        runCurrent()
        val state = viewModel.uiState.value as LoginUiState.Failed
        assertEquals(LoginFailureReason.POLL_FAILED, state.reason)
        assertTrue(signedIn.isEmpty())
    }

    @Test
    fun `expiry budget fails the flow without polling forever`() = runTest {
        gateway.startResult = Result.success(
            gateway.startResult.getOrThrow().copy(expiresIn = 10),
        )
        val viewModel = createViewModel()
        runCurrent()
        viewModel.selectProvider("github")
        runCurrent()

        advanceTimeBy(5_000)
        runCurrent()
        assertEquals(1, gateway.pollCalls)

        advanceTimeBy(5_000)
        runCurrent()
        val state = viewModel.uiState.value as LoginUiState.Failed
        assertEquals(LoginFailureReason.EXPIRED, state.reason)
        assertEquals(1, gateway.pollCalls)
    }

    @Test
    fun `invalid session fails after completion`() = runTest {
        gateway.pollResults += Result.success(DevicePollResult.Complete("sess-9"))
        gateway.sessionResult = Result.success(null)
        val viewModel = createViewModel()
        runCurrent()
        viewModel.selectProvider("github")
        runCurrent()

        advanceTimeBy(5_000)
        runCurrent()
        val state = viewModel.uiState.value as LoginUiState.Failed
        assertEquals(LoginFailureReason.SESSION_INVALID, state.reason)
        assertTrue(signedIn.isEmpty())
    }

    @Test
    fun `cancel aborts polling and returns to the picker`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        viewModel.selectProvider("github")
        runCurrent()
        assertEquals(LoginUiState.Verifying("ABCD-1234"), viewModel.uiState.value)

        viewModel.cancel()
        runCurrent()
        assertTrue(viewModel.uiState.value is LoginUiState.ProviderPicker)

        advanceTimeBy(60_000)
        runCurrent()
        assertEquals("no polls after cancel", 0, gateway.pollCalls)
    }
}
