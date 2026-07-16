package social.waddle.android.feature.login

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.LocalAppGraph
import social.waddle.android.TestAppGraph
import social.waddle.android.testAuthProvider

/**
 * Device-authorization login screen against the scriptable fake
 * gateway. The device-start fixtures carry no verification URI, so no
 * Custom Tab ever launches over the test activity.
 */
@RunWith(AndroidJUnit4::class)
class LoginScreenTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private lateinit var harness: TestAppGraph

    @Before
    fun setUp() {
        harness = TestAppGraph()
    }

    @After
    fun tearDown() {
        harness.shutdown()
    }

    private fun composeLoginScreen() {
        composeRule.setContent {
            CompositionLocalProvider(LocalAppGraph provides harness.graph) {
                LoginScreen()
            }
        }
    }

    private fun waitForText(text: String) {
        composeRule.waitUntil(timeoutMillis = 10_000) {
            composeRule.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
        }
    }

    @Test
    fun providersRenderAsButtons() {
        harness.loginGateway.providersResult = Result.success(
            listOf(
                testAuthProvider(id = "github", displayName = "GitHub"),
                testAuthProvider(id = "google", displayName = "Google"),
            ),
        )
        composeLoginScreen()
        waitForText("Continue with GitHub")
        composeRule.onNodeWithText("Continue with Google").assertIsDisplayed()
    }

    @Test
    fun providerWithoutDisplayNameFallsBackToItsId() {
        harness.loginGateway.providersResult = Result.success(
            listOf(testAuthProvider(id = "gitea", displayName = null)),
        )
        composeLoginScreen()
        waitForText("Continue with gitea")
    }

    @Test
    fun emptyProviderListShowsNoProvidersState() {
        harness.loginGateway.providersResult = Result.success(emptyList())
        composeLoginScreen()
        waitForText("No sign-in providers are configured on this server.")
    }

    @Test
    fun selectingProviderShowsUserCodeAndCancelReturnsToPicker() {
        composeLoginScreen()
        waitForText("Continue with GitHub")
        composeRule.onNodeWithText("Continue with GitHub").performClick()
        waitForText("ABCD-1234")
        composeRule.onNodeWithText("Cancel").performClick()
        waitForText("Continue with GitHub")
    }

    @Test
    fun providerLoadFailureShowsErrorAndRetryRecovers() {
        harness.loginGateway.providersResult = Result.failure(RuntimeException("providers down"))
        composeLoginScreen()
        waitForText("Couldn't load the sign-in providers.")
        harness.loginGateway.providersResult = Result.success(listOf(testAuthProvider()))
        composeRule.onNodeWithText("Try again").performClick()
        waitForText("Continue with GitHub")
    }

    @Test
    fun deviceStartFailureShowsFailedAndRetryReturnsToPicker() {
        harness.loginGateway.startResult = Result.failure(RuntimeException("start refused"))
        composeLoginScreen()
        waitForText("Continue with GitHub")
        composeRule.onNodeWithText("Continue with GitHub").performClick()
        waitForText("Couldn't start the sign-in.")
        composeRule.onNodeWithText("Try again").performClick()
        waitForText("Continue with GitHub")
    }
}
