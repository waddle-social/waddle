package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import social.waddle.android.R
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScores

/**
 * Compact marker for the highest visible safety category. Colour and icon
 * describe the category, while probability only controls visibility.
 */
@Composable
fun SafetyScoresChip(category: WaddleSafetyCategory, onClick: () -> Unit) {
    val label = stringResource(safetyScoreLabelRes(category))
    val description = stringResource(R.string.safety_scores_chip_description, label)
    val style = safetyScoreStyle(category)
    val tint = style.color()
    Surface(
        shape = RoundedCornerShape(10.dp),
        color = MaterialTheme.colorScheme.surface,
        onClick = onClick,
        modifier = Modifier
            .padding(top = 2.dp)
            .semantics { contentDescription = description },
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
        ) {
            Icon(
                style.icon,
                contentDescription = null,
                tint = tint,
                modifier = Modifier.size(14.dp),
            )
            Text(
                text = label,
                style = MaterialTheme.typography.labelSmall,
                color = tint,
                modifier = Modifier.padding(start = 4.dp),
            )
        }
    }
}

/** Breakdown of the categories at or above the visibility threshold. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SafetyScoresSheet(
    scores: WaddleSafetyScores,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            verticalArrangement = Arrangement.spacedBy(12.dp),
            // Several categories plus copy can outgrow a short screen or
            // large font scale; the sheet content must scroll.
            modifier = Modifier
                .navigationBarsPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 24.dp)
                .padding(bottom = 16.dp),
        ) {
            Text(
                text = stringResource(R.string.safety_scores_title),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                text = stringResource(R.string.safety_scores_explainer),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            safetyScoreRowsOf(scores).forEach { row -> SafetyScoreLine(row) }
            Text(
                text = stringResource(R.string.safety_scores_model, scores.modelVersion),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun SafetyScoreLine(row: SafetyScoreRow) {
    val style = safetyScoreStyle(row.category)
    val tint = style.color()
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                imageVector = style.icon,
                contentDescription = null,
                tint = tint,
                modifier = Modifier.size(18.dp),
            )
            Text(
                text = stringResource(safetyScoreLabelRes(row.category)),
                style = MaterialTheme.typography.bodyMedium,
                color = tint,
                modifier = Modifier.weight(1f).padding(start = 6.dp),
            )
            Text(
                text = stringResource(R.string.safety_scores_percent, row.percent),
                style = MaterialTheme.typography.labelLarge,
            )
        }
        LinearProgressIndicator(
            progress = { row.fraction },
            color = tint,
            modifier = Modifier.fillMaxWidth(),
        )
        Text(
            text = stringResource(R.string.safety_scores_taxonomy, row.taxonomyVersion),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
