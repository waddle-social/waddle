package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.client.ffi.WaddleLinkPreviewLookup
import social.waddle.client.ffi.WaddleLinkPreviewLookupPreview
import social.waddle.client.ffi.WaddleLinkPreviewLookupStatus

class LinkPreviewComposerTest {

    private fun readyLookup(expiresAt: String, token: String = "tok-1") = WaddleLinkPreviewLookup(
        status = WaddleLinkPreviewLookupStatus.READY,
        preview = WaddleLinkPreviewLookupPreview(
            token = token,
            originalUrl = "https://example.com/a",
            normalizedUrl = "https://example.com/a",
            expiresAt = expiresAt,
            title = null,
            description = null,
            image = null,
            playerEmbed = null,
        ),
    )

    @Test
    fun `first eligible url skips plain text and http`() {
        assertNull(firstEligibleHttpsUrl("no links here"))
        assertNull(firstEligibleHttpsUrl("http://insecure.example/x"))
        assertEquals(
            "https://example.com/a",
            firstEligibleHttpsUrl("see https://example.com/a and https://example.com/b"),
        )
    }

    @Test
    fun `first eligible url trims wrapping punctuation`() {
        assertEquals(
            "https://example.com/a",
            firstEligibleHttpsUrl("(https://example.com/a)."),
        )
    }

    @Test
    fun `hosts without a dot are ineligible`() {
        assertNull(firstEligibleHttpsUrl("https://localhost/admin"))
    }

    @Test
    fun `fresh ready lookup yields its token`() {
        val lookup = readyLookup(expiresAt = "2026-07-16T12:00:00Z")
        val nowMs = 1_500_000_000_000L // long before the expiry
        assertEquals("tok-1", linkPreviewSendToken(lookup, nowMs))
    }

    @Test
    fun `expired token is never attached`() {
        val lookup = readyLookup(expiresAt = "2020-01-01T00:00:00Z")
        assertNull(linkPreviewSendToken(lookup, System.currentTimeMillis()))
    }

    @Test
    fun `unparsable expiry is never attached`() {
        val lookup = readyLookup(expiresAt = "not a timestamp")
        assertNull(linkPreviewSendToken(lookup, 0L))
    }

    @Test
    fun `non ready statuses yield no token`() {
        for (status in listOf(
            WaddleLinkPreviewLookupStatus.UNSUPPORTED,
            WaddleLinkPreviewLookupStatus.BLOCKED,
            WaddleLinkPreviewLookupStatus.FAILED,
        )) {
            assertNull(linkPreviewSendToken(WaddleLinkPreviewLookup(status, preview = null), 0L))
        }
    }

    @Test
    fun `null lookup yields no token`() {
        assertNull(linkPreviewSendToken(null, 0L))
    }
}
