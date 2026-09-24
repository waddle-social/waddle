import Foundation

/// How a slash-invoked extension command runs, given the trailing text.
public enum ExtensionInvocation: Hashable, Sendable {
    /// Submit the command's form with `field` set to `value`.
    case inlineSubmit(field: String, value: String)
    /// Execute directly without showing a form.
    case execute
    /// Show the command's form, pre-filling its first required field.
    case openForm(prefill: String?)

    /// Mirrors the web `buildSlashInvocation`: inline submit when the
    /// command takes its argument inline and one was typed; direct execute
    /// for a bare composer-execute command; otherwise the form, keeping any
    /// typed text as a prefill.
    public init(command: ExtensionCommand, trailing: String) {
        let value = trailing.trimmingCharacters(in: .whitespacesAndNewlines)
        if let field = command.inlineField, !value.isEmpty {
            self = .inlineSubmit(field: field, value: value)
        } else if command.composerExecute, value.isEmpty {
            self = .execute
        } else if command.inlineField == nil, !value.isEmpty {
            self = .openForm(prefill: value)
        } else {
            self = .openForm(prefill: nil)
        }
    }
}
