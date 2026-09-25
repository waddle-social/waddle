package social.waddle.android.feature.conversation

import androidx.annotation.StringRes
import social.waddle.android.R
import social.waddle.client.ffi.WaddleSafetyScoreCategory
import social.waddle.client.ffi.WaddleSafetyScoresPayload
import kotlin.math.roundToInt

/** One line of the XEP-0422 safety-scores breakdown sheet. */
data class SafetyScoreRow(
    val category: WaddleSafetyScoreCategory,
    /** Whole percent, 0..100, for the label. */
    val percent: Int,
    /** 0f..1f, for the bar. */
    val fraction: Float,
    val taxonomyVersion: String,
)

/**
 * Breakdown rows in the fixed category order (the server's judgment
 * table order), independent of wire order.
 */
fun safetyScoreRowsOf(scores: WaddleSafetyScoresPayload.Scores): List<SafetyScoreRow> =
    scores.scores
        .sortedBy { it.category.ordinal }
        .map { score ->
            val fraction = score.probability.coerceIn(0.0, 1.0)
            SafetyScoreRow(
                category = score.category,
                percent = (fraction * PERCENT).roundToInt(),
                fraction = fraction.toFloat(),
                taxonomyVersion = score.taxonomyVersion,
            )
        }

/** The row affordance shows only when there is something to open. */
fun hasSafetyScoreBreakdown(scores: WaddleSafetyScoresPayload.Scores?): Boolean =
    scores != null && scores.scores.isNotEmpty()

@StringRes
fun safetyScoreLabelRes(category: WaddleSafetyScoreCategory): Int = when (category) {
    WaddleSafetyScoreCategory.IS_QUESTION -> R.string.safety_score_is_question
    WaddleSafetyScoreCategory.HATE_SPEECH -> R.string.safety_score_hate_speech
    WaddleSafetyScoreCategory.EXPLICIT -> R.string.safety_score_explicit
    WaddleSafetyScoreCategory.HARASSMENT -> R.string.safety_score_harassment
    WaddleSafetyScoreCategory.VIOLENCE -> R.string.safety_score_violence
    WaddleSafetyScoreCategory.SELF_HARM -> R.string.safety_score_self_harm
}

private const val PERCENT = 100.0
