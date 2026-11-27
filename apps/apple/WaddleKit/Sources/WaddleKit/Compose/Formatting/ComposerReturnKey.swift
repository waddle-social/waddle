/// Routes unmodified Return after the native field reports its keyboard and IME state.
public enum ComposerReturnKey: Equatable, Sendable {
    public enum Keyboard: CaseIterable, Sendable {
        case hardware, onScreen
    }

    case useNative
    case suggestion
    case edit(ComposerTextEdit)
    case submit

    public static func route(
        text: String, selection: ComposerScalarRange?, hasMarkedText: Bool, keyboard: Keyboard,
        takeSuggestion: () -> Bool
    ) -> Self {
        guard !hasMarkedText else { return .useNative }
        if keyboard == .hardware, takeSuggestion() { return .suggestion }
        if case let .edit(edit) = ComposerReturn.decide(text: text, selection: selection) { return .edit(edit) }
        return keyboard == .hardware ? .submit : .useNative
    }
}
