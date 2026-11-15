package social.waddle.android.feature.events

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PlanningMoneyTest {
    @Test fun `GBP displays pounds and parses pennies exactly`() {
        assertEquals(1250L, planningAmountMinor("12.50", "GBP"))
        assertEquals(1L, planningAmountMinor("0.01", "GBP"))
        assertEquals(0L, planningAmountMinor("0", "GBP"))
        assertEquals("12.50", planningAmountText(1250L, "GBP"))
        assertNull(planningAmountMinor("12.501", "GBP"))
        assertEquals(1250L, planningAmountMinor("12.500", "GBP"))
    }

    @Test fun `JPY amounts have no fractional units`() {
        assertEquals(1250L, planningAmountMinor("1250", "JPY"))
        assertEquals("1250", planningAmountText(1250L, "JPY"))
        assertNull(planningAmountMinor("12.50", "JPY"))
        assertEquals(12L, planningAmountMinor("12.0", "JPY"))
    }

    @Test fun `three decimal currencies preserve every fractional unit`() {
        assertEquals(12345L, planningAmountMinor("12.345", "KWD"))
        assertEquals("12.345", planningAmountText(12345L, "KWD"))
        assertEquals("0.001", planningAmountText(1L, "KWD"))
        assertNull(planningAmountMinor("12.3451", "KWD"))
        assertEquals(12345L, planningAmountMinor("12.3450", "KWD"))
    }

    @Test fun `invalid negative and unsafe amounts are rejected`() {
        listOf("-1", "-0.01", "NaN", "1e3", "", "1,000.00").forEach {
            assertNull(planningAmountMinor(it, "GBP"))
        }
        assertNull(planningAmountMinor("1", "INVALID"))
        assertNull(planningAmountMinor("1", "XXX"))
        assertEquals(9_007_199_254_740_991L, planningAmountMinor("90071992547409.91", "GBP"))
        assertNull(planningAmountMinor("90071992547409.92", "GBP"))
        assertNull(planningAmountMinor("99999999999999999999999999999", "JPY"))
    }
}
