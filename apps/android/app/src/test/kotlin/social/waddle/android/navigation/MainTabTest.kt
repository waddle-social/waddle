package social.waddle.android.navigation

import org.junit.Assert.assertEquals
import org.junit.Test

class MainTabTest {
    @Test
    fun `tabs are home, rooms, people, dms, you`() {
        assertEquals(
            listOf(MainTab.HOME, MainTab.ROOMS, MainTab.PEOPLE, MainTab.DMS, MainTab.YOU),
            MainTab.entries.toList(),
        )
    }

    @Test
    fun `a saved tab restores by name`() {
        MainTab.entries.forEach { assertEquals(it, MainTab.fromName(it.name)) }
    }

    @Test
    fun `a tab that no longer exists restores to home instead of crashing`() {
        // ACTIVITY was a tab before Huddle; a state saved then must still restore.
        assertEquals(MainTab.HOME, MainTab.fromName("ACTIVITY"))
        assertEquals(MainTab.HOME, MainTab.fromName(null))
        assertEquals(MainTab.HOME, MainTab.fromName(""))
    }
}
