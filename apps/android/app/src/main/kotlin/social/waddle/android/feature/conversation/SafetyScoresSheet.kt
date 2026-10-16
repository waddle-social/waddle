package social.waddle.android.feature.conversation

import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.android.theme.Waddle
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScores

/**
 * Compact marker for the highest visible safety category: a quiet
 * outlined tag on the message's 28dp rhythm. Colour and icon describe
 * the category, while probability only controls visibility.
 */
@Composable
fun SafetyScoresChip(category: WaddleSafetyCategory, onClick: () -> Unit) {
    val label = stringResource(safetyScoreLabelRes(category))
    val description = stringResource(R.string.safety_scores_chip_description, label)
    val style = safetyScoreStyle(category)
    val tint = style.color()
    val shape = RoundedCornerShape(Waddle.radius.tag)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Waddle.space.xxs),
        modifier = Modifier
            .padding(top = Waddle.space.xxs)
            .height(Waddle.control.xs)
            .clip(shape)
            .border(1.dp, Waddle.colors.border, shape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = description }
            .padding(horizontal = Waddle.space.xs),
    ) {
        Icon(style.icon, contentDescription = null, tint = tint, modifier = Modifier.size(CHIP_ICON))
        Text(text = label, style = MetaStyle, color = tint)
    }
}

/** Breakdown of the categories at or above the visibility threshold. */
@Composable
fun SafetyScoresSheet(
    scores: WaddleSafetyScores,
    onDismiss: () -> Unit,
) {
    ConversationSheet(onDismissRequest = onDismiss, title = stringResource(R.string.safety_scores_title)) {
        // Several categories plus copy can outgrow a short screen or
        // large font scale; the sheet content must scroll.
        Column(
            verticalArrangement = Arrangement.spacedBy(Waddle.space.md),
            modifier = Modifier.verticalScroll(rememberScrollState()),
        ) {
            Text(
                text = stringResource(R.string.safety_scores_explainer),
                style = MetaStyle,
                color = Waddle.colors.fgMuted,
            )
            safetyScoreRowsOf(scores).forEach { row -> SafetyScoreLine(row) }
            Text(
                text = stringResource(R.string.safety_scores_model, scores.modelVersion),
                style = Waddle.type.time,
                color = Waddle.colors.fgMuted,
            )
        }
    }
}

@Composable
private fun SafetyScoreLine(row: SafetyScoreRow) {
    val style = safetyScoreStyle(row.category)
    val tint = style.color()
    Column(verticalArrangement = Arrangement.spacedBy(Waddle.space.xxs)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Waddle.space.xs),
        ) {
            Icon(imageVector = style.icon, contentDescription = null, tint = tint, modifier = Modifier.size(LINE_ICON))
            Text(
                text = stringResource(safetyScoreLabelRes(row.category)),
                style = Waddle.type.control,
                color = Waddle.colors.fg,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = stringResource(R.string.safety_scores_percent, row.percent),
                style = Waddle.type.time,
                color = tint,
            )
        }
        LinearProgressIndicator(
            progress = { row.fraction },
            color = tint,
            trackColor = Waddle.colors.surface2,
            drawStopIndicator = {},
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(Waddle.radius.tag)),
        )
        Text(
            text = stringResource(R.string.safety_scores_taxonomy, row.taxonomyVersion),
            style = Waddle.type.time,
            color = Waddle.colors.fgMuted,
        )
    }
}

private val CHIP_ICON = 14.dp
private val LINE_ICON = 18.dp
