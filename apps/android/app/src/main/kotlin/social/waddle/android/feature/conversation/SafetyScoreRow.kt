package social.waddle.android.feature.conversation

import androidx.annotation.StringRes
import social.waddle.android.R
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScore
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

const val SAFETY_SCORE_VISIBILITY_THRESHOLD = 0.5

/** Community signals are listed apart from safety categories and never raise the chip. */
fun WaddleSafetyCategory.isSafety(): Boolean = this != WaddleSafetyCategory.IS_QUESTION

/**
 * The highest visible safety category drives the chip. Ties use the fixed
 * category order so wire order cannot change its colour or icon. Questions
 * appear in the breakdown but never drive the safety chip.
 */
fun safetyScoreChipCategory(scores: WaddleSafetyScores?): WaddleSafetyCategory? =
    scores?.scores
        ?.filter { it.category.isSafety() && it.probability >= SAFETY_SCORE_VISIBILITY_THRESHOLD }
        ?.sortedWith(compareByDescending<WaddleSafetyScore> { it.probability }.thenBy { it.category.ordinal })
        ?.firstOrNull()
        ?.category

/**
 * Breakdown rows in the fixed category order (the server's judgment
 * table order), independent of wire order. Only categories at or above
 * the visibility threshold are listed; the rest stay hidden.
 */
fun safetyScoreRowsOf(scores: WaddleSafetyScores): List<SafetyScoreRow> =
    scores.scores
        .filter { it.probability >= SAFETY_SCORE_VISIBILITY_THRESHOLD }
        .sortedBy { it.category.ordinal }
        .map(::safetyScoreRowOf)

private fun safetyScoreRowOf(score: WaddleSafetyScore): SafetyScoreRow {
    val fraction = score.probability.coerceIn(0.0, 1.0)
    return SafetyScoreRow(
        category = score.category,
        percent = (fraction * PERCENT).roundToInt(),
        fraction = fraction.toFloat(),
        taxonomyVersion = score.taxonomyVersion,
    )
}

@StringRes
fun safetyScoreLabelRes(category: WaddleSafetyCategory): Int = when (category) {
    WaddleSafetyCategory.IS_QUESTION -> R.string.safety_score_is_question
    WaddleSafetyCategory.HATE_SPEECH -> R.string.safety_score_hate_speech
    WaddleSafetyCategory.EXPLICIT -> R.string.safety_score_explicit
    WaddleSafetyCategory.HARASSMENT -> R.string.safety_score_harassment
    WaddleSafetyCategory.VIOLENCE -> R.string.safety_score_violence
    WaddleSafetyCategory.SELF_HARM -> R.string.safety_score_self_harm
    WaddleSafetyCategory.SPAM -> R.string.safety_score_spam
    WaddleSafetyCategory.SCAM -> R.string.safety_score_scam
}

private const val PERCENT = 100.0
