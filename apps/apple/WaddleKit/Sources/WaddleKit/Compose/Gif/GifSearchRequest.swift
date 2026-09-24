import Foundation

/// The Waddle web app's `/api/giphy` proxy, which holds the Giphy key.
public enum GifSearchRequest {
    public static let defaultBase = URL(string: "https://waddle.chat")!
    public static let defaultLimit = 24
    static let limitRange = 1...50
    static let maxQueryLength = 100

    /// `<base>/api/giphy?limit=N&q=…`; an empty query asks for trending
    /// GIFs and omits `q`.
    public static func url(base: URL = defaultBase, query: String, limit: Int = defaultLimit) -> URL {
        var components = URLComponents(url: base.appendingPathComponent("api/giphy"), resolvingAgainstBaseURL: false)!
        let clamped = min(max(limit, limitRange.lowerBound), limitRange.upperBound)
        var items = [URLQueryItem(name: "limit", value: String(clamped))]
        let trimmed = String(query.trimmingCharacters(in: .whitespacesAndNewlines).prefix(maxQueryLength))
        if !trimmed.isEmpty {
            items.append(URLQueryItem(name: "q", value: encoded(trimmed)))
        }
        components.percentEncodedQueryItems = items
        return components.url!
    }

    /// Percent-encodes a query value, including `+`, `&`, `=` and `#`,
    /// which a form-decoding server would otherwise misread.
    private static func encoded(_ value: String) -> String {
        var allowed = CharacterSet.urlQueryAllowed
        allowed.remove(charactersIn: "+&=#")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
    }
}
