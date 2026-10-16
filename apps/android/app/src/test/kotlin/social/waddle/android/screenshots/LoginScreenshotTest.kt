package social.waddle.android.screenshots

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.client.auth.AuthProvider
import social.waddle.android.feature.login.LoginContent
import social.waddle.android.feature.login.LoginUiState

/** First impression: the sign-in surface, and the device-code step. */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi", application = Application::class)
class LoginScreenshotTest {
    @Test
    fun login() = captureThemed("login") {
        LoginContent(
            state = LoginUiState.ProviderPicker(
                providers = listOf(
                    AuthProvider(id = "github", kind = "oidc", displayName = "GitHub"),
                    AuthProvider(id = "google", kind = "oidc", displayName = "Google"),
                ),
            ),
            onSelectProvider = {},
            onCancel = {},
            onDismissError = {},
        )
    }
}
