package social.waddle.android.feature.conversation

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material.icons.outlined.FavoriteBorder
import androidx.compose.material.icons.outlined.GppBad
import androidx.compose.material.icons.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.PersonOff
import androidx.compose.material.icons.outlined.SmsFailed
import androidx.compose.material.icons.outlined.VisibilityOff
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.vector.ImageVector
import social.waddle.client.ffi.WaddleSafetyCategory

/** Category meaning stays constant as probability and theme change. */
data class SafetyScoreStyle(
    val icon: ImageVector,
    val lightColor: Color,
    val darkColor: Color,
)

fun safetyScoreStyle(category: WaddleSafetyCategory): SafetyScoreStyle = when (category) {
    WaddleSafetyCategory.IS_QUESTION -> SafetyScoreStyle(
        Icons.Outlined.HelpOutline, Color(0xFF2563EB), Color(0xFF60A5FA),
    )
    WaddleSafetyCategory.HATE_SPEECH -> SafetyScoreStyle(
        Icons.Outlined.PersonOff, Color(0xFFDC2626), Color(0xFFF87171),
    )
    WaddleSafetyCategory.EXPLICIT -> SafetyScoreStyle(
        Icons.Outlined.VisibilityOff, Color(0xFF7C3AED), Color(0xFFA78BFA),
    )
    WaddleSafetyCategory.HARASSMENT -> SafetyScoreStyle(
        Icons.Outlined.SmsFailed, Color(0xFFC2410C), Color(0xFFFB923C),
    )
    WaddleSafetyCategory.VIOLENCE -> SafetyScoreStyle(
        Icons.Outlined.Bolt, Color(0xFFBE123C), Color(0xFFFB7185),
    )
    WaddleSafetyCategory.SELF_HARM -> SafetyScoreStyle(
        Icons.Outlined.FavoriteBorder, Color(0xFF0F766E), Color(0xFF2DD4BF),
    )
    WaddleSafetyCategory.SPAM -> SafetyScoreStyle(
        Icons.Outlined.Inbox, Color(0xFF475569), Color(0xFFCBD5E1),
    )
    WaddleSafetyCategory.SCAM -> SafetyScoreStyle(
        Icons.Outlined.GppBad, Color(0xFFB45309), Color(0xFFFBBF24),
    )
}

/** Use the active Material scheme so manual light/dark preference is respected. */
@Composable
fun SafetyScoreStyle.color(): Color =
    if (MaterialTheme.colorScheme.background.luminance() < 0.5f) darkColor else lightColor
