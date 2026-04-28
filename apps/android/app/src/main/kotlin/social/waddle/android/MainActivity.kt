package social.waddle.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.DisposableEffect
import org.koin.compose.koinInject
import social.waddle.android.connection.WaddleConnectionManager
import social.waddle.android.domain.auth.toWaddleConfig
import social.waddle.android.ui.WaddleRoot
import social.waddle.android.ui.auth.AuthGate
import social.waddle.android.ui.theme.WaddleTheme

internal class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.auto(0, 0),
            navigationBarStyle = SystemBarStyle.auto(0, 0),
        )
        super.onCreate(savedInstanceState)
        setContent {
            WaddleTheme {
                AuthGate { signedIn, onSignOut ->
                    val connection = koinInject<WaddleConnectionManager>()
                    DisposableEffect(signedIn.session.sessionId) {
                        connection.start(signedIn.session.toWaddleConfig())
                        onDispose { connection.stop() }
                    }
                    WaddleRoot(
                        username = signedIn.session.username,
                        jid = signedIn.session.jid,
                        onSignOut = onSignOut,
                    )
                }
            }
        }
    }
}
