/// In the plain-text draft, paired markdown code markers are the equivalent
/// of the web editor's active code mark or code block.
enum RoomSuggestionCodeContext {
    private enum FencePosition {
        case outside
        case opener
        case content
    }

    static func overlapsCode(_ range: Range<Int>, in scalars: [Unicode.Scalar]) -> Bool {
        let lineStart = scalars[..<range.lowerBound].lastIndex(of: "\n").map { $0 + 1 } ?? 0
        let lineEnd = scalars[range.upperBound...].firstIndex(of: "\n") ?? scalars.endIndex
        switch fencePosition(of: lineStart..<lineEnd, in: scalars) {
        // The opener's line is dropped whole when the fence is composed, so a link picked on it would be lost.
        case .opener, .content: return true
        case .outside: break
        }

        // An inline code span cannot cross a line break. Only run the marker
        // matcher when backticks on this line could enclose the trigger.
        guard scalars[lineStart..<range.lowerBound].contains("`"),
              scalars[range.upperBound..<lineEnd].contains("`") else { return false }
        let local = (range.lowerBound - lineStart)..<(range.upperBound - lineStart)
        return inlineCodeOverlaps(local, in: Array(scalars[lineStart..<lineEnd]))
    }

    private static func fencePosition(of targetLine: Range<Int>, in scalars: [Unicode.Scalar]) -> FencePosition {
        var isOpen = false
        var start = scalars.startIndex
        while start < targetLine.lowerBound {
            let end = scalars[start..<targetLine.lowerBound].firstIndex(of: "\n") ?? targetLine.lowerBound
            let line = scalars[start..<end]
            isOpen = isOpen ? !closesMarkdownFence(line) : opensMarkdownFence(line)
            start = end + 1
        }
        guard isOpen || opensMarkdownFence(scalars[targetLine]) else { return .outside }

        start = targetLine.upperBound + 1
        while start < scalars.endIndex {
            let end = scalars[start...].firstIndex(of: "\n") ?? scalars.endIndex
            if closesMarkdownFence(scalars[start..<end]) { return isOpen ? .content : .opener }
            start = end + 1
        }
        return .outside
    }
}
