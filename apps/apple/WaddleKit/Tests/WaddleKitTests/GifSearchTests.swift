import Foundation
import Testing
@testable import WaddleKit

struct GifSearchTests {
    private func data(_ json: String) -> Data { Data(json.utf8) }

    private func entry(_ id: String, preview: String, original: String, title: String? = "Cat") -> String {
        let titleField = title.map { "\"title\":\"\($0)\"," } ?? ""
        return """
        {"id":"\(id)",\(titleField)"images":{"fixed_height_small":{"url":"\(preview)"},"original":{"url":"\(original)"}}}
        """
    }

    @Test func decodesResults() {
        let body = "{\"data\":[\(entry("a", preview: "https://media.giphy.com/a/small.gif", original: "https://media.giphy.com/a/giphy.gif"))]}"
        let result = GifSearchResponse.parse(data: data(body), statusCode: 200)
        #expect(result == .results([
            GifSearchItem(
                id: "a",
                title: "Cat",
                previewURL: URL(string: "https://media.giphy.com/a/small.gif")!,
                originalURL: URL(string: "https://media.giphy.com/a/giphy.gif")!
            ),
        ]))
    }

    @Test func skipsMalformedAndNonHTTPSEntries() {
        let entries = [
            entry("ok", preview: "https://x.test/p.gif", original: "https://x.test/o.gif", title: nil),
            entry("http", preview: "http://x.test/p.gif", original: "https://x.test/o.gif"),
            entry("js", preview: "https://x.test/p.gif", original: "javascript:alert(1)"),
            "{\"id\":\"noimages\"}",
            "{\"id\":7,\"images\":{\"fixed_height_small\":{\"url\":\"https://x.test/p.gif\"},\"original\":{\"url\":\"https://x.test/o.gif\"}}}",
            "\"not an object\"",
        ]
        let result = GifSearchResponse.parse(data: data("{\"data\":[\(entries.joined(separator: ","))]}"), statusCode: 200)
        guard case let .results(items) = result else {
            Issue.record("expected results, got \(result)")
            return
        }
        #expect(items.map(\.id) == ["ok"])
        #expect(items.first?.title == "")
    }

    @Test func emptyOrUnreadableSuccess() {
        #expect(GifSearchResponse.parse(data: data("{}"), statusCode: 200) == .results([]))
        #expect(GifSearchResponse.parse(data: data("<html>"), statusCode: 200) == .unavailable(message: "GIF search is unavailable"))
    }

    @Test func notConfigured() {
        let body = data("{\"error\":\"GIF search is not configured\"}")
        #expect(GifSearchResponse.parse(data: body, statusCode: 503) == .notConfigured)
    }

    @Test func rateLimitedCarriesServerMessage() {
        let body = data("{\"error\":\"Too many GIF searches — try again shortly\"}")
        #expect(GifSearchResponse.parse(data: body, statusCode: 429) == .unavailable(message: "Too many GIF searches — try again shortly"))
    }

    @Test func otherFailuresFallBackToDefaultMessage() {
        #expect(GifSearchResponse.parse(data: Data(), statusCode: 502) == .unavailable(message: "GIF search is unavailable"))
        #expect(GifSearchResponse.parse(data: data("{\"error\":5}"), statusCode: 500) == .unavailable(message: "GIF search is unavailable"))
    }

    @Test func requestURL() {
        #expect(GifSearchRequest.url(query: "").absoluteString == "https://waddle.chat/api/giphy?limit=24")
        #expect(GifSearchRequest.url(query: "  happy cat ").absoluteString == "https://waddle.chat/api/giphy?limit=24&q=happy%20cat")
        #expect(GifSearchRequest.url(query: "c++ & a=b #1").absoluteString
            == "https://waddle.chat/api/giphy?limit=24&q=c%2B%2B%20%26%20a%3Db%20%231")
    }

    @Test func requestClampsLimitAndCapsQuery() {
        let base = URL(string: "https://example.test")!
        #expect(GifSearchRequest.url(base: base, query: "", limit: 0).absoluteString == "https://example.test/api/giphy?limit=1")
        #expect(GifSearchRequest.url(base: base, query: "", limit: 500).absoluteString == "https://example.test/api/giphy?limit=50")
        let long = String(repeating: "a", count: 150)
        let url = GifSearchRequest.url(base: base, query: long, limit: 10)
        #expect(url.absoluteString == "https://example.test/api/giphy?limit=10&q=" + String(repeating: "a", count: 100))
    }
}
