import Foundation

/// The XEP-0085 composing line above the composer.
enum TypingText {
    static func sentence(for names: [String]) -> String? {
        switch names.count {
        case 0:
            return nil
        case 1:
            return "\(names[0]) is typing…"
        case 2:
            return "\(names[0]) and \(names[1]) are typing…"
        default:
            return "Several people are typing…"
        }
    }
}
