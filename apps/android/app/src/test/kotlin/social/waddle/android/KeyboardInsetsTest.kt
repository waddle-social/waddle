package social.waddle.android

import android.app.Application
import android.content.ComponentName
import android.view.WindowManager
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Compose handles the keyboard itself (`imePadding()` on the composer).
 * Without adjustResize the system also pans the window, so the keyboard
 * is applied twice: the top bar slides under the status bar and a
 * keyboard-sized gap opens under the composer.
 */
@RunWith(AndroidJUnit4::class)
@Config(sdk = [35], application = Application::class)
class KeyboardInsetsTest {
    @Test
    fun `the main activity leaves the keyboard to compose`() {
        val context = ApplicationProvider.getApplicationContext<Application>()
        val info = context.packageManager.getActivityInfo(ComponentName(context, MainActivity::class.java), 0)

        assertEquals(
            WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE,
            info.softInputMode and WindowManager.LayoutParams.SOFT_INPUT_MASK_ADJUST,
        )
    }
}
