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
) {
    /** This row's own severity; null below the notice threshold. */
    val severity: SafetyScoreSeverity? get() = safetyScoreSeverityOf(fraction.toDouble())
}

/**
 * How loudly a score is surfaced. Below [NOTICE_THRESHOLD] a score is not
 * surfaced at all; the chip is amber from [NOTICE_THRESHOLD] and red from
 * [ALERT_THRESHOLD].
 */
enum class SafetyScoreSeverity { NOTICE, ALERT }

const val NOTICE_THRESHOLD = 0.5
const val ALERT_THRESHOLD = 0.8

/** Severity of one probability; null below the notice threshold. */
fun safetyScoreSeverityOf(probability: Double): SafetyScoreSeverity? = when {
    probability >= ALERT_THRESHOLD -> SafetyScoreSeverity.ALERT
    probability >= NOTICE_THRESHOLD -> SafetyScoreSeverity.NOTICE
    else -> null
}

/** Community signals are listed apart from safety categories and never raise the chip. */
fun WaddleSafetyCategory.isSafety(): Boolean = this != WaddleSafetyCategory.IS_QUESTION

/**
 * The chip's severity: that of the highest content-safety score, or null
 * when no safety category reaches the notice threshold (then there is no
 * chip). A near-certain question is a community signal, not a warning.
 */
fun safetyScoreSeverity(scores: WaddleSafetyScores?): SafetyScoreSeverity? =
    scores?.scores
        ?.filter { it.category.isSafety() }
        ?.mapNotNull { safetyScoreSeverityOf(it.probability) }
        ?.maxOrNull()

/**
 * Breakdown rows in the fixed category order (the server's judgment
 * table order), independent of wire order. Only categories at or above
 * the notice threshold are listed; the rest stay hidden.
 */
fun safetyScoreRowsOf(scores: WaddleSafetyScores): List<SafetyScoreRow> =
    scores.scores
        .filter { safetyScoreSeverityOf(it.probability) != null }
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

@StringRes
fun safetyScoreChipDescriptionRes(severity: SafetyScoreSeverity): Int = when (severity) {
    SafetyScoreSeverity.NOTICE -> R.string.safety_scores_chip_notice
    SafetyScoreSeverity.ALERT -> R.string.safety_scores_chip_alert
}

private const val PERCENT = 100.0
