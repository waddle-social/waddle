package social.waddle.android.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material3.ButtonGroupDefaults
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItemColors
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.SwitchColors
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextFieldColors
import androidx.compose.material3.ToggleButton
import androidx.compose.material3.ToggleButtonDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.theme.Waddle

/**
 * A titled group of rows on a Huddle card (settings, profile, You):
 * kicker above, a card (radius 16, 1dp border) below, rows separated by
 * [GroupDivider]. The card is the only container: rows inside never get
 * a second card.
 */
@Composable
fun GroupedSection(
    title: String?,
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    val colors = Waddle.colors
    Column(modifier = modifier.fillMaxWidth()) {
        title?.let { SectionKicker(text = it) }
        Surface(
            shape = RoundedCornerShape(Waddle.radius.card),
            color = colors.surface,
            contentColor = colors.fg,
            border = BorderStroke(1.dp, colors.border),
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
        color = Waddle.colors.borderSoft,
        modifier = modifier.padding(start = Waddle.space.md),
    )
}

/** ListItem colours for rows on a [GroupedSection] card. */
@Composable
fun groupedRowColors(): ListItemColors {
    val colors = Waddle.colors
    return ListItemDefaults.colors(
        containerColor = Color.Transparent,
        headlineColor = colors.fg,
        supportingColor = colors.fgMuted,
        leadingIconColor = colors.fgMuted,
        trailingIconColor = colors.fgMuted,
    )
}

/**
 * One row on a [GroupedSection] card: optional leading slot, a title in
 * body type with an optional muted line under it, and a trailing slot
 * (a chevron, a switch, a tag). At least 56dp tall; tappable when
 * [onClick] is set. Every settings-like surface uses it so rows share
 * one rhythm.
 */
@Composable
fun GroupedRow(
    title: String,
    modifier: Modifier = Modifier,
    supporting: String? = null,
    leading: (@Composable () -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
    titleColor: Color = Waddle.colors.fg,
    onClick: (() -> Unit)? = null,
) {
    val colors = Waddle.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.sm),
        modifier = modifier
            .fillMaxWidth()
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .heightIn(min = GROUPED_ROW)
            .padding(horizontal = Waddle.space.md, vertical = Waddle.space.sm),
    ) {
        leading?.invoke()
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
            Text(text = title, style = Waddle.type.body, color = titleColor)
            supporting?.let {
                Text(text = it, style = Waddle.type.control.copy(fontWeight = null), color = colors.fgMuted)
            }
        }
        trailing?.invoke()
    }
}

/** The trailing chevron of a row that opens somewhere. */
@Composable
fun RowChevron() {
    Icon(
        Icons.AutoMirrored.Outlined.KeyboardArrowRight,
        contentDescription = null,
        tint = Waddle.colors.fgSoft,
        modifier = Modifier.size(CHEVRON),
    )
}

/**
 * Huddle switch colours, one recipe in both themes: on is a teal knob
 * on a teal-washed track, off is a soft knob on the raised surface.
 * Never a near-black knob on a bright track.
 */
@Composable
fun huddleSwitchColors(): SwitchColors {
    val colors = Waddle.colors
    return SwitchDefaults.colors(
        checkedThumbColor = colors.action,
        checkedTrackColor = colors.actionSubtle,
        checkedBorderColor = colors.action,
        checkedIconColor = colors.actionFg,
        uncheckedThumbColor = colors.fgSoft,
        uncheckedTrackColor = colors.surface2,
        uncheckedBorderColor = colors.border,
        uncheckedIconColor = colors.surface2,
    )
}

/** Huddle text fields: radius 10, quiet border, teal focus, readable labels. */
@Composable
fun huddleFieldColors(): TextFieldColors {
    val colors = Waddle.colors
    return OutlinedTextFieldDefaults.colors(
        focusedTextColor = colors.fg,
        unfocusedTextColor = colors.fg,
        focusedBorderColor = colors.action,
        unfocusedBorderColor = colors.border,
        focusedLabelColor = colors.actionText,
        unfocusedLabelColor = colors.fgMuted,
        focusedPlaceholderColor = colors.fgSoft,
        unfocusedPlaceholderColor = colors.fgSoft,
        focusedLeadingIconColor = colors.fgMuted,
        unfocusedLeadingIconColor = colors.fgMuted,
        cursorColor = colors.action,
        focusedContainerColor = Color.Transparent,
        unfocusedContainerColor = Color.Transparent,
        errorBorderColor = colors.dangerText,
        errorLabelColor = colors.dangerText,
        errorSupportingTextColor = colors.dangerText,
    )
}

/** The field shape that goes with [huddleFieldColors]. */
val HuddleFieldShape = RoundedCornerShape(Waddle.radius.control)

/** One choice of a [SegmentedChoice]. */
data class ChoiceOption<out T>(val value: T, val label: String, val icon: ImageVector? = null)

/**
 * A single-choice control as Material 3 Expressive's connected button
 * group: equal-width toggle buttons joined by a 2dp seam, the chosen one
 * filled teal and rounded fully. It stands on its own (never inside a
 * second outline) and exposes radio-group semantics.
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun <T> SegmentedChoice(
    options: List<ChoiceOption<T>>,
    selected: T,
    onSelect: (T) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = Waddle.colors
    Row(
        horizontalArrangement = Arrangement.spacedBy(ButtonGroupDefaults.ConnectedSpaceBetween),
        modifier = modifier
            .fillMaxWidth()
            .selectableGroup(),
    ) {
        options.forEachIndexed { index, option ->
            val checked = option.value == selected
            ToggleButton(
                checked = checked,
                onCheckedChange = { onSelect(option.value) },
                shapes = when (index) {
                    0 -> ButtonGroupDefaults.connectedLeadingButtonShapes()
                    options.lastIndex -> ButtonGroupDefaults.connectedTrailingButtonShapes()
                    else -> ButtonGroupDefaults.connectedMiddleButtonShapes()
                },
                colors = ToggleButtonDefaults.colors(
                    containerColor = colors.surface2,
                    contentColor = colors.fg,
                    checkedContainerColor = colors.action,
                    checkedContentColor = colors.actionFg,
                ),
                modifier = Modifier
                    .weight(1f)
                    .heightIn(min = Waddle.control.touch)
                    .semantics { this.selected = checked },
            ) {
                option.icon?.let {
                    Icon(it, contentDescription = null, modifier = Modifier.size(ToggleButtonDefaults.IconSize))
                    Spacer(Modifier.size(ToggleButtonDefaults.IconSpacing))
                }
                Text(text = option.label, style = Waddle.type.control, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}

private val GROUPED_ROW = 56.dp
private val CHEVRON = 20.dp
