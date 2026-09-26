package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.R
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScore
import social.waddle.client.ffi.WaddleSafetyScores

class SafetyScoreRowTest {
    private fun scoresOf(vararg scores: WaddleSafetyScore) =
        WaddleSafetyScores(modelVersion = "typesafe/jev-1.13-20260917", scores = scores.toList())

    @Test
    fun `a probability is a notice from half and an alert from four fifths`() {
        assertNull(safetyScoreSeverityOf(0.49))
        assertEquals(SafetyScoreSeverity.NOTICE, safetyScoreSeverityOf(0.5))
        assertEquals(SafetyScoreSeverity.NOTICE, safetyScoreSeverityOf(0.79))
        assertEquals(SafetyScoreSeverity.ALERT, safetyScoreSeverityOf(0.8))
        assertEquals(SafetyScoreSeverity.ALERT, safetyScoreSeverityOf(1.0))
    }

    @Test
    fun `the chip severity follows the highest safety score only`() {
        assertNull(safetyScoreSeverity(null))
        assertNull(safetyScoreSeverity(scoresOf()))
        assertNull(
            safetyScoreSeverity(
                scoresOf(
                    WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.49, "v"),
                    // A near-certain question is a community signal, never a warning.
                    WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 1.0, "v"),
                ),
            ),
        )
        assertEquals(
            SafetyScoreSeverity.NOTICE,
            safetyScoreSeverity(
                scoresOf(
                    WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.5, "v"),
                    WaddleSafetyScore(WaddleSafetyCategory.SPAM_SCAM, 0.49, "v"),
                ),
            ),
        )
        assertEquals(
            SafetyScoreSeverity.ALERT,
            safetyScoreSeverity(
                scoresOf(
                    WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.5, "v"),
                    WaddleSafetyScore(WaddleSafetyCategory.SPAM_SCAM, 0.8, "v"),
                ),
            ),
        )
    }

    @Test
    fun `rows keep only notice-level categories in the fixed order regardless of wire order`() {
        val rows = safetyScoreRowsOf(
            scoresOf(
                WaddleSafetyScore(WaddleSafetyCategory.SPAM_SCAM, 0.8, "safety-spam-scam-v1"),
                WaddleSafetyScore(WaddleSafetyCategory.SELF_HARM, 0.0, "safety-self-harm-v1"),
                WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 0.92, "is-question-v1"),
                WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.5, "safety-harassment-v1"),
                WaddleSafetyScore(WaddleSafetyCategory.HATE_SPEECH, 0.49, "safety-hate-speech-v1"),
            ),
        )

        assertEquals(
            listOf(
                WaddleSafetyCategory.IS_QUESTION,
                WaddleSafetyCategory.HARASSMENT,
                WaddleSafetyCategory.SPAM_SCAM,
            ),
            rows.map { it.category },
        )
        assertEquals(
            SafetyScoreRow(WaddleSafetyCategory.IS_QUESTION, 92, 0.92f, "is-question-v1"),
            rows.first(),
        )
        assertEquals(
            listOf(SafetyScoreSeverity.ALERT, SafetyScoreSeverity.NOTICE, SafetyScoreSeverity.ALERT),
            rows.map { it.severity },
        )
    }

    @Test
    fun `percentages round to whole numbers`() {
        val rows = safetyScoreRowsOf(
            scoresOf(
                WaddleSafetyScore(WaddleSafetyCategory.VIOLENCE, 0.505, "v"),
                WaddleSafetyScore(WaddleSafetyCategory.EXPLICIT, 0.994, "v"),
                WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 1.0, "v"),
            ),
        )

        assertEquals(listOf(99, 100, 51), rows.map { it.percent })
    }

    @Test
    fun `every category has its own label`() {
        val labels = WaddleSafetyCategory.entries.map(::safetyScoreLabelRes)

        assertEquals(WaddleSafetyCategory.entries.size, labels.toSet().size)
        assertEquals(R.string.safety_score_self_harm, safetyScoreLabelRes(WaddleSafetyCategory.SELF_HARM))
        assertEquals(R.string.safety_score_spam_scam, safetyScoreLabelRes(WaddleSafetyCategory.SPAM_SCAM))
    }

    @Test
    fun `each severity has its own chip description`() {
        assertEquals(R.string.safety_scores_chip_notice, safetyScoreChipDescriptionRes(SafetyScoreSeverity.NOTICE))
        assertEquals(R.string.safety_scores_chip_alert, safetyScoreChipDescriptionRes(SafetyScoreSeverity.ALERT))
    }
}
