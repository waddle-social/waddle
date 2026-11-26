package social.waddle.android.screenshots

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import com.github.takahirom.roborazzi.captureRoboImage
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.theme.WaddleTheme

/** Render [content] on the theme's ground in both schemes and record it. */
internal fun captureThemed(name: String, content: @Composable () -> Unit) {
    listOf(ThemeMode.DAYLIGHT to "light", ThemeMode.NIGHT to "dark").forEach { (mode, suffix) ->
        captureRoboImage("src/test/screenshots/${name}_$suffix.png") {
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
