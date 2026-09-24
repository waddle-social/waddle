import Foundation

extension ExtensionCommandField {
    /// Whether the field satisfies XEP-0004 `<required/>`. `fixed` fields
    /// carry no input and always do.
    var hasRequiredValue: Bool {
        switch type {
        case .fixed:
            return true
        case .jidMulti, .listMulti, .textMulti:
            return values.contains(where: Self.isFilled)
        case .boolean, .hidden, .jidSingle, .listSingle, .textPrivate, .textSingle:
            return values.first.map(Self.isFilled) ?? false
        }
    }

    /// The field as submitted: single-valued types send their first
    /// value; `fixed` fields are display-only and send nothing. Blank
    /// `jid-multi` lines are dropped, since an empty value is not a JID.
    var submittedValue: ExtensionFormValue? {
        switch type {
        case .fixed:
            return nil
        case .jidMulti:
            return ExtensionFormValue(variable: variable, values: values.filter(Self.isFilled))
        case .hidden, .listMulti, .textMulti:
            return ExtensionFormValue(variable: variable, values: values)
        case .boolean, .jidSingle, .listSingle, .textPrivate, .textSingle:
            return ExtensionFormValue(variable: variable, values: Array(values.prefix(1)))
        }
    }

    private static func isFilled(_ value: String) -> Bool {
        !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}
