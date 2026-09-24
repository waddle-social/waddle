import Foundation

/// One submitted field: `var` and its `<value/>` children.
public struct ExtensionFormValue: Sendable, Equatable, Hashable {
    public let variable: String
    public let values: [String]

    public init(variable: String, values: [String]) {
        self.variable = variable
        self.values = values
    }
}
