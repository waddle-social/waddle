package social.waddle.android.avatar

import androidx.activity.ComponentActivity
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The decoded-avatar holder is keyed by (JID, item id): a new id or a
 * slot reused for another person must never keep the previous bitmap.
 */
@RunWith(AndroidJUnit4::class)
class PeerAvatarImageSwapTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun changingTheKeySwapsTheImageAndNeverShowsTheOldOne() {
        val images = mapOf(
            "alice@waddle.test#id-1" to ImageBitmap(1, 1),
            "alice@waddle.test#id-2" to ImageBitmap(2, 2),
            "bob@waddle.test#id-9" to ImageBitmap(3, 3),
        )
        var cacheKey by mutableStateOf("alice@waddle.test#id-1")
        val seen = mutableListOf<Pair<String, ImageBitmap?>>()
        composeRule.setContent {
            val key = cacheKey
            val image = rememberDecodedAvatar(key, cached = { null }) { images[it] }
            seen += key to image
        }
        composeRule.waitForIdle()
        assertSame(images["alice@waddle.test#id-1"], seen.last().second)

        // AvatarChanged: same person, new id.
        composeRule.runOnIdle { cacheKey = "alice@waddle.test#id-2" }
        composeRule.waitForIdle()
        assertSame(images["alice@waddle.test#id-2"], seen.last().second)

        // Slot reused for another person.
        composeRule.runOnIdle { cacheKey = "bob@waddle.test#id-9" }
        composeRule.waitForIdle()
        assertSame(images["bob@waddle.test#id-9"], seen.last().second)

        // No frame ever paired a key with another key's bitmap.
        seen.forEach { (key, image) ->
            if (image != null) assertSame(images[key], image)
        }
        assertNotSame(seen.first().second, seen.last().second)
    }
}
