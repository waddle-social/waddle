package social.waddle.android.feature.conversation

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material.icons.outlined.FavoriteBorder
import androidx.compose.material.icons.outlined.GppBad
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.PersonOff
import androidx.compose.material.icons.outlined.SmsFailed
import androidx.compose.material.icons.outlined.VisibilityOff
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import social.waddle.android.theme.Waddle
import social.waddle.client.ffi.WaddleSafetyCategory

/**
 * Which theme role a safety category is drawn in. Meanings, not
 * decoration: harm is danger, risk is warning, a question is teal (it
 * invites an answer), spam is muted, self-harm is gold (care, not
 * alarm). Never ember: ember is only for live things.
 */
enum class SafetyTone { ACTION, DANGER, WARNING, GOLD, MUTED }

/** Category meaning stays constant as probability and theme change; the icon tells categories of one tone apart. */
data class SafetyScoreStyle(
    val icon: ImageVector,
    val tone: SafetyTone,
)

fun safetyScoreStyle(category: WaddleSafetyCategory): SafetyScoreStyle = when (category) {
    WaddleSafetyCategory.IS_QUESTION -> SafetyScoreStyle(Icons.AutoMirrored.Outlined.HelpOutline, SafetyTone.ACTION)
    WaddleSafetyCategory.HATE_SPEECH -> SafetyScoreStyle(Icons.Outlined.PersonOff, SafetyTone.DANGER)
    WaddleSafetyCategory.EXPLICIT -> SafetyScoreStyle(Icons.Outlined.VisibilityOff, SafetyTone.DANGER)
    WaddleSafetyCategory.HARASSMENT -> SafetyScoreStyle(Icons.Outlined.SmsFailed, SafetyTone.WARNING)
    WaddleSafetyCategory.VIOLENCE -> SafetyScoreStyle(Icons.Outlined.Bolt, SafetyTone.DANGER)
    WaddleSafetyCategory.SELF_HARM -> SafetyScoreStyle(Icons.Outlined.FavoriteBorder, SafetyTone.GOLD)
    WaddleSafetyCategory.SPAM -> SafetyScoreStyle(Icons.Outlined.Inbox, SafetyTone.MUTED)
    WaddleSafetyCategory.SCAM -> SafetyScoreStyle(Icons.Outlined.GppBad, SafetyTone.WARNING)
}

/** The readable text colour of this style's tone in the current theme (night or daylight). */
@Composable
@ReadOnlyComposable
fun SafetyScoreStyle.color(): Color {
    val colors = Waddle.colors
    return when (tone) {
        SafetyTone.ACTION -> colors.actionText
        SafetyTone.DANGER -> colors.dangerText
        SafetyTone.WARNING -> colors.warningText
        SafetyTone.GOLD -> colors.goldText
        SafetyTone.MUTED -> colors.fgMuted
    }
}
