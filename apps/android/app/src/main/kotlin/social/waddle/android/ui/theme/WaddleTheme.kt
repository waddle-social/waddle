package social.waddle.android.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext

private val LightFallback = lightColorScheme(
    primary = WaddleSlate,
    onPrimary = WaddleParchment,
    secondary = WaddleAccent,
    onSecondary = WaddleInk,
    background = WaddleParchment,
    onBackground = WaddleInk,
    surface = WaddleParchment,
    onSurface = WaddleInk,
)

private val DarkFallback = darkColorScheme(
    primary = WaddleAccent,
    onPrimary = WaddleInk,
    secondary = WaddleSlate,
    onSecondary = WaddleParchment,
    background = WaddleInk,
    onBackground = WaddleParchment,
    surface = WaddleSlate,
    onSurface = WaddleParchment,
)

@Composable
internal fun WaddleTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val scheme = when {
        dynamicColor && darkTheme -> dynamicDarkColorScheme(context)
        dynamicColor && !darkTheme -> dynamicLightColorScheme(context)
        darkTheme -> DarkFallback
        else -> LightFallback
    }
    MaterialTheme(
        colorScheme = scheme,
        typography = WaddleTypography,
        content = content,
    )
}
