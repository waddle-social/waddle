import WaddleKit

/// Connects native editor keys to the composer's actions.
@MainActor
struct ComposerKeyInput {
    let suggestion: (ComposerHardwareKey) -> Bool
    let returnKey: (String, ComposerScalarRange?, Bool, ComposerReturnKey.Keyboard) -> Bool
    let format: (ComposerFormat) -> Void
    let submit: () -> Void
    let cancel: () -> Bool

    init(
        suggestion: @escaping (ComposerHardwareKey) -> Bool, edit: @escaping (ComposerTextEdit) -> Void,
        format: @escaping (ComposerFormat) -> Void, submit: @escaping () -> Void, cancel: @escaping () -> Bool
    ) {
        self.suggestion = suggestion
        self.format = format
        self.submit = submit
        self.cancel = cancel
        returnKey = { text, selection, hasMarkedText, keyboard in
            switch ComposerReturnKey.route(text: text, selection: selection, hasMarkedText: hasMarkedText, keyboard: keyboard,
                                           takeSuggestion: { suggestion(.returnKey) }) {
            case .useNative: return false
            case .suggestion: break
            case let .edit(change): edit(change)
            case .submit: submit()
            }
            return true
        }
    }
}
