package social.waddle.android.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

private val DefaultStack = FontFamily.Default

internal val WaddleTypography = Typography().run {
    copy(
        displayLarge = displayLarge.brand(FontWeight.SemiBold),
        displayMedium = displayMedium.brand(FontWeight.SemiBold),
        displaySmall = displaySmall.brand(FontWeight.SemiBold),
        headlineLarge = headlineLarge.brand(FontWeight.SemiBold),
        headlineMedium = headlineMedium.brand(FontWeight.SemiBold),
        headlineSmall = headlineSmall.brand(FontWeight.SemiBold),
        titleLarge = titleLarge.brand(FontWeight.Medium, lineHeightShift = 2),
        titleMedium = titleMedium.brand(FontWeight.Medium),
        titleSmall = titleSmall.brand(FontWeight.Medium),
        bodyLarge = bodyLarge.brand(FontWeight.Normal, lineHeightShift = 1),
        bodyMedium = bodyMedium.brand(FontWeight.Normal),
        bodySmall = bodySmall.brand(FontWeight.Normal),
        labelLarge = labelLarge.brand(FontWeight.Medium),
        labelMedium = labelMedium.brand(FontWeight.Medium),
        labelSmall = labelSmall.brand(FontWeight.Medium),
    )
}

private fun TextStyle.brand(weight: FontWeight, lineHeightShift: Int = 0): TextStyle = copy(
    fontFamily = DefaultStack,
    fontWeight = weight,
    lineHeight = if (lineHeightShift == 0) lineHeight else (lineHeight.value + lineHeightShift).sp,
)
