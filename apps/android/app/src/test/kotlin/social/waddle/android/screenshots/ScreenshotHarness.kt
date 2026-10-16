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

/**
 * A deterministic stand-in for the Rust XEP-0392 hue (whose native
 * library JVM tests cannot load): stable per name, spread over the wheel.
 */
internal fun stableTestHue(input: String): Double = Math.floorMod(input.hashCode() * HUE_SPREAD, FULL_TURN).toDouble()

private const val HUE_SPREAD = 2_654_435
private const val FULL_TURN = 360

/** Render [content] on the theme's ground in both schemes and record it. */
internal fun captureThemed(name: String, content: @Composable () -> Unit) {
    listOf(ThemeMode.LIGHT to "light", ThemeMode.DARK to "dark").forEach { (mode, suffix) ->
        captureRoboImage("src/test/screenshots/${name}_$suffix.png") {
            CompositionLocalProvider(LocalConsistentHue provides ::stableTestHue) {
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
