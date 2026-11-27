package social.waddle.android.feature.login

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
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
import social.waddle.android.client.server.ServerDiscoveryError
import social.waddle.android.client.server.ServerDiscoveryException
import social.waddle.android.client.testSessionInfo
import java.net.SocketTimeoutException

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

    private class FakeServers : LoginServerGateway {
        override val default = "https://waddle.default"
        private val state = MutableStateFlow(default)
        override val current: StateFlow<String> = state
        var resolveResult: Result<String> = Result.success("https://chat.example")
        val resolved = mutableListOf<String>()

        override suspend fun resolve(input: String): Result<String> {
            resolved += input
            return resolveResult
        }

        override suspend fun select(origin: String) {
            state.value = origin
        }
    }

    private val servers = FakeServers()
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
        LoginViewModel(gateway = gateway, servers = servers) { signedIn += it }

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

    @Test
    fun `server row starts on the default server`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        assertEquals(ServerUiState(label = "waddle.default", isCustom = false), viewModel.server.value)
    }

    @Test
    fun `a resolved custom server is selected and providers reload`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        gateway.providersResult = Result.success(listOf(AuthProvider(id = "dev", kind = "dev")))

        viewModel.editServer()
        viewModel.submitServer("chat.example")
        runCurrent()

        assertEquals(listOf("chat.example"), servers.resolved)
        assertEquals("https://chat.example", servers.current.value)
        assertEquals(ServerUiState(label = "chat.example", isCustom = true), viewModel.server.value)
        assertEquals(
            LoginUiState.ProviderPicker(listOf(AuthProvider(id = "dev", kind = "dev"))),
            viewModel.uiState.value,
        )
    }

    @Test
    fun `a failed probe keeps the working server and shows the error`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        servers.resolveResult =
            Result.failure(ServerDiscoveryException(ServerDiscoveryError.NOT_A_WADDLE_SERVER))

        viewModel.editServer()
        viewModel.submitServer("example.com")
        runCurrent()

        assertEquals("https://waddle.default", servers.current.value)
        val server = viewModel.server.value
        assertEquals(ServerDiscoveryError.NOT_A_WADDLE_SERVER, server.error)
        assertTrue(server.editing)
    }

    @Test
    fun `use default returns to the default server`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        viewModel.submitServer("chat.example")
        runCurrent()

        viewModel.useDefaultServer()
        runCurrent()

        assertEquals("https://waddle.default", servers.current.value)
        assertEquals(false, viewModel.server.value.isCustom)
    }
}
