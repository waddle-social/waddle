package social.waddle.android.feature.conversation

import androidx.annotation.StringRes
import social.waddle.android.R
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScores
import kotlin.math.roundToInt

/** One line of the XEP-0422 safety-scores breakdown sheet. */
data class SafetyScoreRow(
    val category: WaddleSafetyCategory,
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
fun safetyScoreRowsOf(scores: WaddleSafetyScores): List<SafetyScoreRow> =
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
fun hasSafetyScoreBreakdown(scores: WaddleSafetyScores?): Boolean =
    scores != null && scores.scores.isNotEmpty()

@StringRes
fun safetyScoreLabelRes(category: WaddleSafetyCategory): Int = when (category) {
    WaddleSafetyCategory.IS_QUESTION -> R.string.safety_score_is_question
    WaddleSafetyCategory.HATE_SPEECH -> R.string.safety_score_hate_speech
    WaddleSafetyCategory.EXPLICIT -> R.string.safety_score_explicit
    WaddleSafetyCategory.HARASSMENT -> R.string.safety_score_harassment
    WaddleSafetyCategory.VIOLENCE -> R.string.safety_score_violence
    WaddleSafetyCategory.SELF_HARM -> R.string.safety_score_self_harm
}

private const val PERCENT = 100.0
