package social.waddle.android

import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import androidx.test.ext.junit.rules.ActivityScenarioRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import io.flutter.embedding.android.FlutterView
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Boots the real host, Rust library and Flutter engine, and waits for a Dart frame. */
@RunWith(AndroidJUnit4::class)
class AppLaunchTest {
    @get:Rule val activityRule = ActivityScenarioRule(MainActivity::class.java)

    @Test
    fun flutterDisplaysFirstFrame() {
        val deadline = SystemClock.uptimeMillis() + 20_000
        var displayed = false
        while (!displayed && SystemClock.uptimeMillis() < deadline) {
            activityRule.scenario.onActivity { activity ->
                displayed = hasFlutterFrame(activity.window.decorView)
            }
            if (!displayed) SystemClock.sleep(100)
        }
        assertTrue("Flutter did not render its first frame within 20 seconds", displayed)
    }

    private fun hasFlutterFrame(view: View): Boolean =
        when (view) {
            is FlutterView -> view.hasRenderedFirstFrame()
            is ViewGroup -> (0 until view.childCount).any { hasFlutterFrame(view.getChildAt(it)) }
            else -> false
        }
}
