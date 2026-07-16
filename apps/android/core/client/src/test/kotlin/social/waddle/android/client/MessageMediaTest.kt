package social.waddle.android.client

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MessageMediaTest {

    @Test
    fun `image extensions match with query and fragment tails`() {
        assertTrue(isImageUrl("https://example.com/pic.png"))
        assertTrue(isImageUrl("https://example.com/pic.JPEG?x=1"))
        assertTrue(isImageUrl("https://example.com/anim.gif#frame"))
        assertTrue(isImageUrl("  https://example.com/pic.webp  "))
    }

    @Test
    fun `giphy media urls match without an extension`() {
        assertTrue(isImageUrl("https://media3.giphy.com/media/abc/giphy.gif"))
        assertTrue(isImageUrl("https://i.giphy.com/abc"))
        assertTrue(isImageUrl("https://media.giphy.com/media/xyz"))
    }

    @Test
    fun `plain text and partial urls do not match`() {
        assertFalse(isImageUrl("look at https://example.com/pic.png"))
        assertFalse(isImageUrl("https://example.com/page.html"))
        assertFalse(isImageUrl("https://giphy.com/gifs/abc"))
        assertFalse(isImageUrl("hello"))
    }
}
