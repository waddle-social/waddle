import Foundation

extension ExtensionCommandForm {
    /// The form with `value` in its first required text or JID field that
    /// is still empty, keeping text typed after a slash command that opens
    /// the form (the web palette's `prefillFirstRequired`). Blocked and
    /// `text-private` fields are never filled. Unchanged when no field fits.
    public func prefillingFirstRequired(with value: String) -> ExtensionCommandForm {
        guard let target = fields.first(where: Self.acceptsPrefill) else { return self }
        return setting([value], for: target.variable)
    }

    private static func acceptsPrefill(_ field: ExtensionCommandField) -> Bool {
        guard field.required, !field.blocked, !field.hasRequiredValue else { return false }
        switch field.type {
        case .textSingle, .textMulti, .jidSingle, .jidMulti:
            return true
        case .boolean, .fixed, .hidden, .listMulti, .listSingle, .textPrivate:
            return false
        }
    }
}
