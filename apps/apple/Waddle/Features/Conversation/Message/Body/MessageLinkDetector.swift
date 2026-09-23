import Foundation

/// Finds bare links in message text with `NSDataDetector`, reported in
/// Unicode scalar offsets like every other range in the body layout. Only
/// web links (http/https with a host) and `mailto:` become tappable; the
/// detector also finds `file:`, `tel:` and app schemes in sender text.
enum MessageLinkDetector {
    private static let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue)

    static func links(in text: String) -> [RichDetectedLink] {
        guard let detector, !text.isEmpty else { return [] }
        let matches = detector.matches(in: text, options: [], range: NSRange(text.startIndex..., in: text))
        return matches.compactMap { match in
            guard let url = match.url, isAllowed(url), let range = Range(match.range, in: text) else { return nil }
            let lower = text.unicodeScalars.distance(from: text.unicodeScalars.startIndex, to: range.lowerBound)
            let upper = lower + text[range].unicodeScalars.count
            return RichDetectedLink(range: lower..<upper, url: url)
        }
    }

    static func isAllowed(_ url: URL) -> Bool {
        switch url.scheme?.lowercased() {
        case "http", "https": return url.host?.isEmpty == false
        case "mailto": return true
        default: return false
        }
    }
}
