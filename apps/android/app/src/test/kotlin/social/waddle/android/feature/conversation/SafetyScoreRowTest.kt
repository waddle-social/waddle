package social.waddle.android.feature.conversation

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material.icons.outlined.FavoriteBorder
import androidx.compose.material.icons.outlined.GppBad
import androidx.compose.material.icons.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.PersonOff
import androidx.compose.material.icons.outlined.SmsFailed
import androidx.compose.material.icons.outlined.VisibilityOff
import androidx.compose.ui.graphics.Color
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
    fun `the chip shows the highest visible safety category and ignores questions`() {
        assertNull(safetyScoreChipCategory(null))
        assertNull(safetyScoreChipCategory(scoresOf()))
        assertNull(
            safetyScoreChipCategory(
                scoresOf(
                    WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.49, "v"),
                    WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 1.0, "v"),
                ),
            ),
        )
        assertEquals(
            WaddleSafetyCategory.HARASSMENT,
            safetyScoreChipCategory(
                scoresOf(
                    WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.5, "v"),
                    WaddleSafetyScore(WaddleSafetyCategory.IS_QUESTION, 1.0, "v"),
                ),
            ),
        )
        assertEquals(
            WaddleSafetyCategory.SCAM,
            safetyScoreChipCategory(
                scoresOf(
                    WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.5, "v"),
                    WaddleSafetyScore(WaddleSafetyCategory.SCAM, 0.8, "v"),
                ),
            ),
        )
    }

    @Test
    fun `chip ties use canonical category order regardless of wire order`() {
        val harassment = WaddleSafetyScore(WaddleSafetyCategory.HARASSMENT, 0.8, "v")
        val scam = WaddleSafetyScore(WaddleSafetyCategory.SCAM, 0.8, "v")
        assertEquals(WaddleSafetyCategory.HARASSMENT, safetyScoreChipCategory(scoresOf(scam, harassment)))
        assertEquals(WaddleSafetyCategory.HARASSMENT, safetyScoreChipCategory(scoresOf(harassment, scam)))
    }

    @Test
    fun `rows keep only categories at or above half in fixed order regardless of wire order`() {
        val rows = safetyScoreRowsOf(
            scoresOf(
                WaddleSafetyScore(WaddleSafetyCategory.SCAM, 0.8, "safety-scam-v1"),
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
                WaddleSafetyCategory.SCAM,
            ),
            rows.map { it.category },
        )
        assertEquals(
            SafetyScoreRow(WaddleSafetyCategory.IS_QUESTION, 92, 0.92f, "is-question-v1"),
            rows.first(),
        )
        assertEquals(listOf(0.92f, 0.5f, 0.8f), rows.map { it.fraction })
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
        assertEquals(R.string.safety_score_spam, safetyScoreLabelRes(WaddleSafetyCategory.SPAM))
        assertEquals(R.string.safety_score_scam, safetyScoreLabelRes(WaddleSafetyCategory.SCAM))
    }

    @Test
    fun `every category has a distinct icon and its own light and dark colour`() {
        val icons = mapOf(
            WaddleSafetyCategory.IS_QUESTION to Icons.Outlined.HelpOutline,
            WaddleSafetyCategory.HATE_SPEECH to Icons.Outlined.PersonOff,
            WaddleSafetyCategory.EXPLICIT to Icons.Outlined.VisibilityOff,
            WaddleSafetyCategory.HARASSMENT to Icons.Outlined.SmsFailed,
            WaddleSafetyCategory.VIOLENCE to Icons.Outlined.Bolt,
            WaddleSafetyCategory.SELF_HARM to Icons.Outlined.FavoriteBorder,
            WaddleSafetyCategory.SPAM to Icons.Outlined.Inbox,
            WaddleSafetyCategory.SCAM to Icons.Outlined.GppBad,
        )
        val styles = WaddleSafetyCategory.entries.map(::safetyScoreStyle)

        assertEquals(WaddleSafetyCategory.entries.size, styles.map { it.icon }.toSet().size)
        assertEquals(WaddleSafetyCategory.entries.size, styles.map { it.lightColor }.toSet().size)
        assertEquals(WaddleSafetyCategory.entries.size, styles.map { it.darkColor }.toSet().size)
        icons.forEach { (category, icon) -> assertEquals(icon, safetyScoreStyle(category).icon) }
        assertEquals(Color(0xFF2563EB), safetyScoreStyle(WaddleSafetyCategory.IS_QUESTION).lightColor)
        assertEquals(Color(0xFF60A5FA), safetyScoreStyle(WaddleSafetyCategory.IS_QUESTION).darkColor)
        assertEquals(Color(0xFFC2410C), safetyScoreStyle(WaddleSafetyCategory.HARASSMENT).lightColor)
        assertEquals(Color(0xFFFB923C), safetyScoreStyle(WaddleSafetyCategory.HARASSMENT).darkColor)
    }
}
