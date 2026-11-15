package social.waddle.android.media

import android.graphics.drawable.Animatable
import android.graphics.drawable.Drawable
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import coil3.asDrawable
import coil3.compose.AsyncImage
import okhttp3.OkHttpClient
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import social.waddle.android.client.EncryptedAttachmentDownloader
import java.util.concurrent.atomic.AtomicReference

/** Decodes actual multi-frame GIF bytes through the same loader used by AsyncImage. */
@RunWith(AndroidJUnit4::class)
class AnimatedGifDecodeTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun sharedLoaderPlaysGifAutomaticallyAndStopsWhenItLeavesComposition() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val httpClient = OkHttpClient()
        val loader = newWaddleImageLoader(context, httpClient, EncryptedAttachmentDownloader(httpClient))
        try {
            val bytes = instrumentation.context.assets.open("animated-two-frame.gif").use { it.readBytes() }
            val decoded = AtomicReference<Drawable?>()
            val visible = mutableStateOf(true)
            composeRule.setContent {
                if (visible.value) {
                    AsyncImage(
                        model = bytes,
                        imageLoader = loader,
                        contentDescription = "Animated test GIF",
                        modifier = Modifier.size(48.dp),
                        onSuccess = { decoded.set(it.result.image.asDrawable(context.resources)) },
                    )
                }
            }
            composeRule.waitUntil(timeoutMillis = 10_000) { decoded.get() != null }
            val drawable = decoded.get()
            assertTrue("GIF decoding must preserve animation", drawable is Animatable)
            composeRule.runOnIdle {
                val animation = drawable as Animatable
                assertTrue("AsyncImage must start GIF playback automatically", animation.isRunning)
                visible.value = false
            }
            composeRule.runOnIdle {
                assertFalse("Removing AsyncImage must stop GIF playback", (drawable as Animatable).isRunning)
            }
        } finally {
            loader.shutdown()
        }
    }
}
