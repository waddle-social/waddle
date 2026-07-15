package social.waddle.android

import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * On-device launch smoke: the process starts, the `.so` loads through
 * JNA, the app-state gate composes, and no crash occurs during the
 * first frames. Network-independent — with no stored session the shell
 * lands on the signed-out surface, which is still tagged.
 */
@RunWith(AndroidJUnit4::class)
class AppLaunchTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<MainActivity>()

    @Test
    fun appShellComposesAfterLaunch() {
        composeRule.waitUntil(timeoutMillis = 20_000) {
            composeRule.onAllNodesWithTag(AppShellTestTags.ROOT)
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
    }
}
