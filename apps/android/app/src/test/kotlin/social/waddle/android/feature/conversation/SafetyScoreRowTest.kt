package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.R
import social.waddle.client.ffi.WaddleSafetyCategory
import social.waddle.client.ffi.WaddleSafetyScore
import social.waddle.client.ffi.WaddleSafetyScores

class SafetyScoreRowTest {
    private fun scoresOf(vararg scores: WaddleSafetyScore) =
        WaddleSafetyScores(modelVersion = "typesafe/jev-1.13-20260917", scores = scores.toList())

    @Test
    fun `rows follow the fixed category order regardless of wire order`() {
        val rows = safetyScoreRowsOf(
            scoresOf(
                WaddleSafetyScore(WaddleSafetyCategory.SELF_HARM, 0.0, "safety-self-harm-v1"),
                WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 0.92, "is-question-v1"),
                WaddleSafetyScore(WaddleSafetyCategory.HATE_SPEECH, 0.03, "safety-hate-speech-v1"),
            ),
        )

        assertEquals(
            listOf(
                WaddleSafetyCategory.IS_QUESTION,
                WaddleSafetyCategory.HATE_SPEECH,
                WaddleSafetyCategory.SELF_HARM,
            ),
            rows.map { it.category },
        )
        assertEquals(
            SafetyScoreRow(WaddleSafetyCategory.IS_QUESTION, 92, 0.92f, "is-question-v1"),
            rows.first(),
        )
    }

    @Test
    fun `percentages round to whole numbers`() {
        val rows = safetyScoreRowsOf(
            scoresOf(
                WaddleSafetyScore(WaddleSafetyCategory.VIOLENCE, 0.005, "v"),
                WaddleSafetyScore(WaddleSafetyCategory.EXPLICIT, 0.994, "v"),
                WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 1.0, "v"),
            ),
        )

        assertEquals(listOf(99, 100, 1), rows.map { it.percent })
    }

    @Test
    fun `the affordance shows only for a non-empty batch`() {
        assertFalse(hasSafetyScoreBreakdown(null))
        assertFalse(hasSafetyScoreBreakdown(scoresOf()))
        assertTrue(
            hasSafetyScoreBreakdown(
                scoresOf(WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 0.5, "v")),
            ),
        )
    }

    @Test
    fun `every category has its own label`() {
        val labels = WaddleSafetyCategory.entries.map(::safetyScoreLabelRes)

        assertEquals(WaddleSafetyCategory.entries.size, labels.toSet().size)
        assertEquals(R.string.safety_score_self_harm, safetyScoreLabelRes(WaddleSafetyCategory.SELF_HARM))
    }
}
