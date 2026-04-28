package social.waddle.android

import android.os.Bundle
import android.util.Log
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
                        // Surface native init errors instead of letting them
                        // crash the composition. The UI remains usable on the
                        // signed-in shell with a Failed connection banner.
                        runCatching {
                            connection.start(signedIn.session.toWaddleConfig())
                        }.onFailure { error ->
                            Log.e(TAG, "WaddleClient failed to start", error)
                        }
                        onDispose {
                            runCatching { connection.stop() }
                                .onFailure { Log.e(TAG, "WaddleClient stop threw", it) }
                        }
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

    private companion object {
        const val TAG = "Waddle"
    }
}
