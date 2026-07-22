package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LinkPreviewMediaTest {

    private val origin = "https://xmpp.waddle.social"
    private val cachedPath = "/api/files/6a1f0f4e-8d24-45a3-9f7a-0b3c8f0d9e2a/" +
        "link-preview-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.png"

    @Test
    fun `trusted cached image url passes`() {
        assertTrue(isTrustedCachedPreviewImageUrl("$origin$cachedPath", origin))
    }

    @Test
    fun `third party hosts never pass`() {
        assertFalse(isTrustedCachedPreviewImageUrl("https://evil.example$cachedPath", origin))
    }

    @Test
    fun `non preview paths never pass`() {
        assertFalse(isTrustedCachedPreviewImageUrl("$origin/api/files/whatever.png", origin))
        assertFalse(
            isTrustedCachedPreviewImageUrl(
                "$origin/api/files/6a1f0f4e-8d24-45a3-9f7a-0b3c8f0d9e2a/link-preview-abc.png",
                origin,
            ),
        )
    }

    @Test
    fun `port mismatches never pass`() {
        assertFalse(isTrustedCachedPreviewImageUrl("$origin:8443$cachedPath", origin))
        // Explicit default port is the same effective port.
        assertTrue(isTrustedCachedPreviewImageUrl("$origin:443$cachedPath", origin))
    }

    @Test
    fun `http is trusted only between loopback hosts`() {
        assertTrue(
            isTrustedCachedPreviewImageUrl(
                "http://localhost$cachedPath",
                "http://localhost",
            ),
        )
        assertFalse(
            isTrustedCachedPreviewImageUrl(
                "http://xmpp.waddle.social$cachedPath",
                "http://xmpp.waddle.social",
            ),
        )
    }

    @Test
    fun `missing trusted origin distrusts everything`() {
        assertFalse(isTrustedCachedPreviewImageUrl("$origin$cachedPath", null))
        assertFalse(isTrustedCachedPreviewImageUrl("$origin$cachedPath", ""))
    }

    @Test
    fun `player embeds allow only the youtube and vimeo players`() {
        assertTrue(isAllowedPlayerEmbedOrigin("https://www.youtube-nocookie.com/embed/x"))
        assertTrue(isAllowedPlayerEmbedOrigin("https://player.vimeo.com/video/1"))
        assertFalse(isAllowedPlayerEmbedOrigin("https://www.youtube.com/embed/x"))
        assertFalse(isAllowedPlayerEmbedOrigin("http://www.youtube-nocookie.com/embed/x"))
        assertFalse(isAllowedPlayerEmbedOrigin("https://www.youtube-nocookie.com:8443/embed/x"))
        assertFalse(isAllowedPlayerEmbedOrigin("https://user:pw@www.youtube-nocookie.com/embed/x"))
    }

    @Test
    fun `hosts render without a www prefix`() {
        assertEquals("example.com", linkPreviewHost("https://www.example.com/story"))
        assertEquals("news.example.com", linkPreviewHost("https://news.example.com/a"))
        assertNull(linkPreviewHost("not a url"))
    }

    @Test
    fun `card taps are https only`() {
        assertEquals("https://example.com/", httpsHrefOrNull("https://example.com/"))
        assertNull(httpsHrefOrNull("http://example.com/"))
        assertNull(httpsHrefOrNull("javascript:alert(1)"))
    }
}
