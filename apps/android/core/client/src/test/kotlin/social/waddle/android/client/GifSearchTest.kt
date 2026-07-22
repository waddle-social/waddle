package social.waddle.android.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class GifSearchTest {

    private val payload = """
        {"data":[
          {"id":"g1","title":"Penguin","images":{
            "fixed_height_small":{"url":"https://media0.giphy.com/g1/200s.gif"},
            "original":{"url":"https://media0.giphy.com/g1/giphy.gif"}}},
          {"id":"g2","images":{
            "fixed_height_small":{"url":"https://media0.giphy.com/g2/200s.gif"},
            "original":{"url":"https://media0.giphy.com/g2/giphy.gif"}}},
          {"id":"broken","images":{}}
        ]}
    """.trimIndent()

    @Test
    fun `success payload parses entries and drops broken ones`() {
        val result = classifyGifSearchResponse(200, payload)
        val results = result as GifSearchResult.Results
        assertEquals(listOf("g1", "g2"), results.gifs.map { it.id })
        assertEquals("Penguin", results.gifs[0].title)
        assertEquals("", results.gifs[1].title)
        assertEquals("https://media0.giphy.com/g1/200s.gif", results.gifs[0].previewUrl)
        assertEquals("https://media0.giphy.com/g1/giphy.gif", results.gifs[0].originalUrl)
    }

    @Test
    fun `status codes map to the web state machine`() {
        assertEquals(GifSearchResult.NotConfigured, classifyGifSearchResponse(503, null))
        assertEquals(GifSearchResult.NotConfigured, classifyGifSearchResponse(404, null))
        assertEquals(GifSearchResult.RateLimited, classifyGifSearchResponse(429, null))
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(502, null))
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(500, null))
    }

    @Test
    fun `malformed success bodies are unavailable`() {
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(200, "not json"))
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(200, "{}"))
        assertEquals(GifSearchResult.Unavailable, classifyGifSearchResponse(200, null))
    }

    @Test
    fun `empty data array is a valid empty result`() {
        val result = classifyGifSearchResponse(200, """{"data":[]}""")
        assertTrue((result as GifSearchResult.Results).gifs.isEmpty())
    }
}
