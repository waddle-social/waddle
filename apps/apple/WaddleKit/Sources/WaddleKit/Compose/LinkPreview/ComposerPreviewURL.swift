import Foundation

extension ComposerLinkPreview {
    /// Scan the same HTTPS tokens and trailing punctuation as the web
    /// composer. Keep the source spelling for the lookup and draft key.
    public nonisolated static func firstEligibleURL(in body: String) -> LinkPreviewURL? {
        eligibleURLs(in: body).first
    }

    nonisolated static func eligibleURLs(in body: String) -> [LinkPreviewURL] {
        body.matches(of: /https:\/\/[^\s<>"']+/.ignoresCase()).compactMap { match in
            let raw = String(match.output).trimmingCharacters(in: CharacterSet(charactersIn: ")]}>.,!?;:"))
            return hasEligibleHost(raw) ? LinkPreviewURL(rawValue: raw) : nil
        }
    }

    nonisolated static func normalizedURL(_ raw: String) -> String {
        guard var url = httpsComponents(raw), let host = url.url?.host(percentEncoded: false) else { return raw }
        url.host = host.lowercased()
        if url.port == 443 { url.port = nil }
        if url.percentEncodedPath.isEmpty { url.percentEncodedPath = "/" }
        return url.url?.standardized.absoluteString ?? raw
    }

    private nonisolated static func httpsComponents(_ raw: String) -> URLComponents? {
        guard raw.lowercased().hasPrefix("https://") else { return nil }
        // Browser HTTPS parsing treats backslashes before the query/fragment
        // like slashes and accepts extra slashes before the host.
        let rest = raw.dropFirst("https://".count)
        let pathEnd = rest.firstIndex { $0 == "?" || $0 == "#" } ?? rest.endIndex
        let authorityAndPath = rest[..<pathEnd]
            .replacingOccurrences(of: "\\", with: "/")
            .drop(while: { $0 == "/" })
        return URLComponents(string: "https://" + authorityAndPath + rest[pathEnd...])
    }

    private nonisolated static func hasEligibleHost(_ raw: String) -> Bool {
        guard let url = httpsComponents(raw),
              let host = url.url?.host(percentEncoded: false),
              !host.isEmpty, !host.hasPrefix("["),
              host.rangeOfCharacter(from: CharacterSet(charactersIn: "#%/:<>?@[\\]^|").union(.whitespacesAndNewlines).union(.controlCharacters)) == nil
        else { return false }
        // Foundation can return a nil port for a number that overflowed.
        if let range = url.rangeOfPort, !range.isEmpty {
            guard let port = url.port, (0...65_535).contains(port) else { return false }
        }

        // WHATWG hosts ending in a number are parsed as IPv4, including
        // shortened, octal, and hexadecimal forms.
        var labels = host.split(separator: ".", omittingEmptySubsequences: false)
        if labels.last?.isEmpty == true { labels.removeLast() }
        if let last = labels.last, !last.isEmpty,
           ipv4Number(last) != nil || last.utf8.allSatisfy({ (48...57).contains($0) }) {
            guard labels.count <= 4 else { return false }
            let numbers = labels.compactMap(ipv4Number)
            guard numbers.count == labels.count, let final = numbers.last else { return false }
            return numbers.dropLast().allSatisfy { $0 <= 255 }
                && final < (UInt64(1) << (8 * (5 - labels.count)))
        }
        return host.contains(".")
    }

    private nonisolated static func ipv4Number(_ label: Substring) -> UInt64? {
        if label.lowercased().hasPrefix("0x") {
            let digits = label.dropFirst(2)
            guard digits.utf8.allSatisfy({
                (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
            }) else { return nil }
            // A syntactically numeric label still ends in a number if it is
            // too large to be an IPv4 address.
            return digits.isEmpty ? 0 : UInt64(digits, radix: 16) ?? .max
        }
        guard label.utf8.first.map({ (48...57).contains($0) }) == true else { return nil }
        return UInt64(label, radix: label.hasPrefix("0") ? 8 : 10)
    }
}
