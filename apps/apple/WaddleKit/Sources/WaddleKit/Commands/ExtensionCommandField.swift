import Foundation

/// One `<option/>` of a list field.
public struct ExtensionFieldOption: Sendable, Equatable, Hashable {
    public let label: String?
    public let value: String

    public init(label: String?, value: String) {
        self.label = label
        self.value = value
    }
}

/// One XEP-0004 `<field/>` of a command-response form. `values` is the
/// only mutable part: the palette edits it before submitting.
public struct ExtensionCommandField: Sendable, Equatable, Hashable {
    /// The `var` attribute; empty only for `fixed` fields.
    public let variable: String
    public let label: String?
    public let type: ExtensionFieldType
    public let required: Bool
    /// A `text-private` or secret-named field. It must never render an
    /// input, and a form carrying one is never submitted forward.
    public let blocked: Bool
    public let options: [ExtensionFieldOption]
    public var values: [String]

    public init(
        variable: String,
        label: String?,
        type: ExtensionFieldType,
        required: Bool,
        blocked: Bool,
        options: [ExtensionFieldOption],
        values: [String]
    ) {
        self.variable = variable
        self.label = label
        self.type = type
        self.required = required
        self.blocked = blocked
        self.options = options
        self.values = values
    }
}
