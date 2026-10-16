package social.waddle.android.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.ListItemColors
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import social.waddle.android.theme.Waddle

/**
 * A titled group of rows on a card (settings, profile, members): kicker
 * above, hairline-bordered card below, rows separated by [GroupDivider].
 */
@Composable
fun GroupedSection(
    title: String?,
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(modifier = modifier.fillMaxWidth()) {
        title?.let { SectionKicker(text = it) }
        Surface(
            shape = RoundedCornerShape(Waddle.radius.panel),
            color = MaterialTheme.colorScheme.surfaceContainer,
            border = BorderStroke(1.dp, Waddle.colors.hairline),
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Waddle.space.md),
        ) {
            Column(content = content)
        }
    }
}

/** Hairline between rows of a [GroupedSection], inset past the leading edge. */
@Composable
fun GroupDivider(modifier: Modifier = Modifier) {
    HorizontalDivider(
        color = Waddle.colors.hairline,
        modifier = modifier.padding(start = Waddle.space.md),
    )
}

/** ListItem colours for rows on a [GroupedSection] card. */
@Composable
fun groupedRowColors(): ListItemColors = ListItemDefaults.colors(
    containerColor = Color.Transparent,
    supportingColor = MaterialTheme.colorScheme.onSurfaceVariant,
    leadingIconColor = MaterialTheme.colorScheme.onSurfaceVariant,
)
