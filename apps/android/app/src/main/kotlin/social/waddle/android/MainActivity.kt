package social.waddle.android

import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleConfig
import social.waddle.client.ffi.WaddleClient
import social.waddle.client.ffi.WaddleEventListener
import social.waddle.client.ffi.parseJid

private const val TAG = "WaddleSpike"

/**
 * M0 toolchain spike: proves the UniFFI chain end-to-end on device —
 * `.so` loading + JNA (sync `parseJid`), the tokio async runtime behind a
 * Kotlin `suspend` call (`connect`), and Rust→Kotlin callbacks (`onEvent`
 * firing on the deliberately unauthenticated connect attempt). Replaced by
 * the real app shell in M1.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    SpikeScreen()
                }
            }
        }
    }
}

@androidx.compose.runtime.Composable
private fun SpikeScreen() {
    var jidResult by remember { mutableStateOf("parsing…") }
    var eventLog by remember { mutableStateOf("connecting…") }

    LaunchedEffect(Unit) {
        val parts = parseJid("spike@waddle.social/android")
        jidResult = "parseJid → local=${parts?.localpart} domain=${parts?.domain}"
        Log.i(TAG, jidResult)

        val listener = object : WaddleEventListener {
            override fun onEvent(event: WaddleClientEvent) {
                Log.i(TAG, "onEvent: $event")
                eventLog = event::class.simpleName ?: event.toString()
            }
        }
        val client = WaddleClient(
            WaddleConfig(
                serverUrl = "wss://xmpp.waddle.social/xmpp-websocket",
                jid = "spike@waddle.social",
                accessToken = "m0-spike-invalid-token",
                resource = "waddle-android-spike",
                resumeState = null,
            ),
            listener,
        )
        // Expected to fail auth — the point is watching Error/Disconnected
        // arrive through the Rust→JNA→Kotlin callback path in logcat.
        client.connect()
    }

    Column(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(text = "Waddle M0 spike", style = MaterialTheme.typography.headlineMedium)
        Text(text = jidResult, style = MaterialTheme.typography.bodyMedium)
        Text(text = "last event: $eventLog", style = MaterialTheme.typography.bodyMedium)
    }
}
