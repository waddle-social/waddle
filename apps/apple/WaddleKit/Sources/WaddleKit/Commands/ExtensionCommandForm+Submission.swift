import Foundation

extension ExtensionCommandForm {
    /// The first field that forbids submitting this form forward.
    public var blockedField: ExtensionCommandField? {
        fields.first(where: \.blocked)
    }

    /// Visible required fields that still lack a value.
    public var missingRequiredFields: [ExtensionCommandField] {
        fields.filter { $0.type != .hidden && $0.required && !$0.hasRequiredValue }
    }

    /// The form with `variable` set to `values`. Blocked and `fixed`
    /// fields are never filled.
    public func setting(_ values: [String], for variable: String) -> ExtensionCommandForm {
        var form = self
        form.fields = fields.map { field in
            guard field.variable == variable, field.type != .fixed, !field.blocked else { return field }
            var filled = field
            filled.values = values
            return filled
        }
        return form
    }

    /// The values `action` submits. A forward action is refused while a
    /// blocked field is present or a required field is empty; `cancel`
    /// and `prev` carry no form.
    func submission(for action: ExtensionCommandAction) throws(ExtensionCommandSubmitError) -> [ExtensionFormValue] {
        guard action.submitsForm else { return [] }
        if let blocked = blockedField {
            throw .forbiddenField(variable: blocked.variable)
        }
        let missing = missingRequiredFields
        guard missing.isEmpty else {
            throw .missingRequiredFields(variables: missing.map(\.variable))
        }
        return fields.compactMap(\.submittedValue)
    }
}
