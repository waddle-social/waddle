import Foundation
import WaddleKit

/// User-facing words for XEP-0050 actions and refused submissions.
enum ExtensionCommandCopy {
    static func title(for action: ExtensionCommandAction) -> String {
        switch action {
        case .execute: return "Run"
        case .next: return "Next"
        case .prev: return "Back"
        case .complete: return "Complete"
        case .cancel: return "Cancel"
        }
    }

    static func message(for error: ExtensionCommandSubmitError, in form: ExtensionCommandForm?) -> String {
        switch error {
        case .forbiddenField:
            return blockedFormMessage
        case let .missingRequiredFields(variables):
            let names = variables.map { variable in form?.field(variable)?.label ?? variable }
            return "Fill in \(names.joined(separator: ", ")) to continue."
        }
    }

    static let blockedFormMessage = "This command asks for a secret value, which Waddle never sends from a form."

    /// The label a field row shows, marking required fields.
    static func label(for field: ExtensionCommandField) -> String {
        let base = field.label ?? field.variable
        return field.required ? "\(base) (required)" : base
    }
}
