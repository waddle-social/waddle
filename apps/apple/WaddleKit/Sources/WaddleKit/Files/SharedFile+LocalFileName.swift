import Foundation

extension SharedFile {
    /// `displayName` as a single local path component: separators become
    /// `_`, names of only dots or whitespace become "attachment", and the
    /// result fits in 200 UTF-8 bytes.
    public var localFileName: String {
        let cleaned = String(displayName.map { "/:\0".contains($0) ? "_" : $0 })
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard cleaned.contains(where: { $0 != "." }) else { return "attachment" }
        var name = ""
        for character in cleaned {
            guard name.utf8.count + character.utf8.count <= 200 else { break }
            name.append(character)
        }
        return name
    }
}
