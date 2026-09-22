import Foundation

/// Finds bare links in message text with `NSDataDetector`, reported in
/// Unicode scalar offsets like every other range in the body layout.
enum MessageLinkDetector {
    private static let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue)

    static func links(in text: String) -> [RichDetectedLink] {
        guard let detector, !text.isEmpty else { return [] }
        let matches = detector.matches(in: text, options: [], range: NSRange(text.startIndex..., in: text))
        return matches.compactMap { match in
            guard let url = match.url, let range = Range(match.range, in: text) else { return nil }
            let lower = text.unicodeScalars.distance(from: text.unicodeScalars.startIndex, to: range.lowerBound)
            let upper = lower + text[range].unicodeScalars.count
            return RichDetectedLink(range: lower..<upper, url: url)
        }
    }
}
