package social.waddle.android.screenshots

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import com.github.takahirom.roborazzi.captureRoboImage
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.theme.LocalConsistentHue
import social.waddle.android.theme.WaddleTheme
import java.security.MessageDigest

/**
 * XEP-0392 §5.1 hue (SHA-1, first two bytes little-endian, scaled to
 * 360°): what the Rust core computes, without its native library.
 */
internal fun xep0392Hue(input: String): Double {
    val digest = MessageDigest.getInstance("SHA-1").digest(input.toByteArray())
    val value = (digest[0].toInt() and 0xFF) or ((digest[1].toInt() and 0xFF) shl 8)
    return value / 65_536.0 * 360.0
}

/** Render [content] on the theme's ground in both schemes and record it. */
internal fun captureThemed(name: String, content: @Composable () -> Unit) {
    listOf(ThemeMode.LIGHT to "light", ThemeMode.DARK to "dark").forEach { (mode, suffix) ->
        captureRoboImage("src/test/screenshots/${name}_$suffix.png") {
            CompositionLocalProvider(LocalConsistentHue provides ::xep0392Hue) {
                WaddleTheme(themeMode = mode) {
                    // The app hosts every screen in a Surface (via Scaffold),
                    // which supplies the content colour text inherits.
                    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                        content()
                    }
                }
            }
        }
    }
}
