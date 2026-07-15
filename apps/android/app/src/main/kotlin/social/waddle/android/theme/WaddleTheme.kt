package social.waddle.android.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import social.waddle.android.client.prefs.ThemeMode

/**
 * Brand accent — the chat `--primary` token (#14B8A6). Kotlin single
 * source of truth for Compose; `res/values/colors.xml`
 * `ic_launcher_background` mirrors the same value for the launcher icon
 * and splash background and must move together with this constant
 * (both get replaced by the generated design-token target later).
 */
private val BrandAccent = Color(0xFF14B8A6)

private val LightColors = lightColorScheme(
    primary = BrandAccent,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFB4F1E6),
    onPrimaryContainer = Color(0xFF00201B),
    secondary = Color(0xFF4A635E),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFCCE8E1),
    onSecondaryContainer = Color(0xFF06201B),
)

private val DarkColors = darkColorScheme(
    primary = BrandAccent,
    onPrimary = Color(0xFF00382F),
    primaryContainer = Color(0xFF005046),
    onPrimaryContainer = Color(0xFF71F7E1),
    secondary = Color(0xFFB1CCC5),
    onSecondary = Color(0xFF1C3531),
    secondaryContainer = Color(0xFF334B47),
    onSecondaryContainer = Color(0xFFCCE8E1),
)

/**
 * Material3 theme around the brand accent. Dynamic color is
 * deliberately OFF (brand consistency with web/Apple); the mode comes
 * from the user pref (`light | system | dark`).
 */
@Composable
fun WaddleTheme(themeMode: ThemeMode, content: @Composable () -> Unit) {
    val darkTheme = when (themeMode) {
        ThemeMode.LIGHT -> false
        ThemeMode.DARK -> true
        ThemeMode.SYSTEM -> isSystemInDarkTheme()
    }
    MaterialTheme(
        colorScheme = if (darkTheme) DarkColors else LightColors,
        content = content,
    )
}
