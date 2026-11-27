import Foundation

extension SharedFile {
    /// `displayName` as a single local path component: separators become
    /// `_`, names of only dots or whitespace become "attachment", and the
    /// result fits in 200 UTF-8 bytes.
    public var localFileName: String {
        // A separator can be part of a grapheme with combining marks.
        let separators = "/:\0".unicodeScalars
        let scalars = displayName.unicodeScalars.map { separators.contains($0) ? "_" : $0 }
        let cleaned = String(String.UnicodeScalarView(scalars))
            .trimmingCharacters(in: .whitespacesAndNewlines)
        var name = ""
        for character in cleaned {
            guard name.utf8.count + character.utf8.count <= 200 else { break }
            name.append(character)
        }
        // Truncation can leave only dots even when the original name has a suffix.
        return name.contains(where: { $0 != "." }) ? name : "attachment"
    }
}
