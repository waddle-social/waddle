package social.waddle.android.feature.events

import android.app.Application
import android.graphics.Bitmap
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Surface
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.SemanticsNodeInteraction
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.isDialog
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import social.waddle.android.client.prefs.ThemeMode
import social.waddle.android.theme.WaddleTheme
import social.waddle.client.ffi.WaddleCalendarDate
import social.waddle.client.ffi.WaddleEvent
import social.waddle.client.ffi.WaddlePlanningKind
import java.io.File
import java.time.Instant
import java.time.LocalDate
import java.util.Locale
import java.util.TimeZone

/** Exercise actual Material dialogs; PNGs are previews, not baseline comparisons. */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "en-rGB-w411dp-h891dp-xxhdpi", application = Application::class)
class EventDatePickerUiTest {
    @get:Rule val compose = createComposeRule()

    private val previousLocale = Locale.getDefault()
    private val previousZone = TimeZone.getDefault()

    @After fun restoreLocale() {
        Locale.setDefault(previousLocale)
        TimeZone.setDefault(previousZone)
    }

    @Before fun pinLocale() {
        Locale.setDefault(Locale.UK)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }

    @Test fun `select a calendar day then a time without typing and clear the optional value`() {
        var result = ""
        compose.setContent {
            var value by remember { mutableStateOf("") }
            WaddleTheme(themeMode = ThemeMode.NIGHT) {
                EventDateTimeField(
                    value, {
                        value = it
                        result = it
                    }, "Start", true,
                    initialPickerDate = LocalDate.of(2027, 2, 12),
                )
            }
        }
        compose.onNodeWithContentDescription("Start date").performClick()
        compose.onNodeWithText("Done").assertIsNotEnabled()
        compose.onNodeWithText("13").performClick()
        preview(compose.onNode(isDialog()), "calendar-night")
        compose.onNodeWithText("Done").performClick()
        compose.onNode(isDialog()).assertIsDisplayed()
        preview(compose.onNode(isDialog()), "time-night")
        compose.onNodeWithText("Done").performClick()
        compose.runOnIdle { assertEquals("2027-02-13T09:00", result) }
        compose.onNodeWithText("13 Feb 2027").assertIsDisplayed()
        compose.onNodeWithContentDescription("Clear Start").performClick()
        compose.runOnIdle { assertEquals("", result) }
        compose.onNodeWithContentDescription("Start time").assertIsNotEnabled()
    }

    @Test fun `opening and cancelling the calendar preserves the original precise time`() {
        val original = "2027-02-12T10:05:37.123"
        var result = original
        val enabled = mutableStateOf(true)
        compose.setContent {
            WaddleTheme(themeMode = ThemeMode.DAYLIGHT) {
                EventDateTimeField(original, { result = it }, "Start", enabled.value)
            }
        }
        compose.onNodeWithContentDescription("Start date").performClick()
        compose.onNodeWithText("Cancel").performClick()
        compose.runOnIdle {
            assertEquals(original, result)
            enabled.value = false
        }
        compose.onNodeWithContentDescription("Start date").assertIsNotEnabled()
        compose.onNodeWithContentDescription("Start time").assertIsNotEnabled()
        compose.onNodeWithContentDescription("Clear Start").assertIsNotEnabled()
    }

    @Test fun `London search selects a valid zone without committing search text`() {
        var result = "UTC"
        compose.setContent {
            var zone by remember { mutableStateOf("UTC") }
            WaddleTheme(themeMode = ThemeMode.NIGHT) {
                EventTimeZoneField(zone, {
                    zone = it
                    result = it
                }, true)
            }
        }
        compose.onNodeWithContentDescription("Time zone").performClick()
        val search = compose.onNodeWithText("Search cities or time zones")
        search.performTextInput("not a valid zone")
        compose.onNodeWithText("No matching time zones.").assertIsDisplayed()
        compose.runOnIdle { assertEquals("UTC", result) }
        search.performTextClearance()
        search.performTextInput("London")
        preview(compose.onNode(isDialog()), "zone-search-night")
        compose.onNodeWithText("London").performClick()
        compose.runOnIdle { assertEquals("Europe/London", result) }
    }

    @Test fun `planning editor displays human dates and saves unchanged instants`() {
        val initial = newPlanningItem(WaddlePlanningKind.EVENT, "London maintainers meetup").copy(
            id = "event", timezone = "Europe/London",
            startsAt = Instant.parse("2027-02-12T10:05:37.123Z").toEpochMilli(),
            endsAt = Instant.parse("2027-02-13T17:00:00Z").toEpochMilli(),
        )
        var saved = initial
        compose.setContent {
            WaddleTheme(themeMode = ThemeMode.NIGHT) {
                Surface {
                    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                        PlanningEditor(initial, busy = false, onCancel = {}, onSave = { saved = it })
                    }
                }
            }
        }
        compose.onNodeWithText("12 Feb 2027").assertIsDisplayed()
        compose.onNodeWithText("2027-02-12T10:05:37.123").assertDoesNotExist()
        preview(compose.onRoot(), "planning-editor-night")
        compose.onNodeWithText("Save").performScrollTo().assertIsEnabled().performClick()
        compose.runOnIdle {
            assertEquals(initial.startsAt, saved.startsAt)
            assertEquals(initial.endsAt, saved.endsAt)
            assertEquals(initial.timezone, saved.timezone)
        }
    }

    @Test fun `title only planning edit keeps both known overlap instants`() {
        val initial = newPlanningItem(WaddlePlanningKind.EVENT, "London trip").copy(
            id = "event", timezone = "Europe/London",
            startsAt = Instant.parse("2027-10-31T00:30:00Z").toEpochMilli(),
            endsAt = Instant.parse("2027-10-31T01:15:00Z").toEpochMilli(),
        )
        var saved = initial
        compose.setContent {
            WaddleTheme(themeMode = ThemeMode.DAYLIGHT) {
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    PlanningEditor(initial, false, {}, { saved = it })
                }
            }
        }
        compose.onNodeWithText("London trip").performTextClearance()
        compose.onNodeWithText("Event name").performTextInput("London meetup")
        compose.onNodeWithText("Save").performScrollTo().assertIsEnabled().performClick()
        compose.runOnIdle {
            assertEquals("London meetup", saved.title)
            assertEquals(initial.startsAt, saved.startsAt)
            assertEquals(initial.endsAt, saved.endsAt)
        }
    }

    @Test fun `title only community edit keeps both known overlap instants`() {
        TimeZone.setDefault(TimeZone.getTimeZone("Europe/London"))
        val initial = WaddleEvent(
            id = "public", uid = "public", summary = "London trip", description = null, location = null,
            organizer = "xmpp:alice@example.test", attendees = emptyList(), workspaceRoom = null,
            start = WaddleCalendarDate.DateTime(Instant.parse("2027-10-31T00:30:00Z").toEpochMilli()),
            end = WaddleCalendarDate.DateTime(Instant.parse("2027-10-31T01:15:00Z").toEpochMilli()),
        )
        var saved = initial
        compose.setContent {
            WaddleTheme(themeMode = ThemeMode.DAYLIGHT) {
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    EventEditor(initial, false, "alice@example.test", {}, { saved = it })
                }
            }
        }
        compose.onNodeWithText("London trip").performTextClearance()
        compose.onNodeWithText("Event name").performTextInput("London meetup")
        compose.onNodeWithText("Save").performScrollTo().assertIsEnabled().performClick()
        compose.runOnIdle {
            assertEquals("London meetup", saved.summary)
            assertEquals(initial.start, saved.start)
            assertEquals(initial.end, saved.end)
        }
    }

    private fun preview(node: SemanticsNodeInteraction, name: String) {
        val destination = File("build/outputs/roborazzi/event-picker-previews/$name.png")
        destination.parentFile!!.mkdirs()
        destination.outputStream().use { stream ->
            check(node.captureToImage().asAndroidBitmap().compress(Bitmap.CompressFormat.PNG, 100, stream))
        }
    }
}
