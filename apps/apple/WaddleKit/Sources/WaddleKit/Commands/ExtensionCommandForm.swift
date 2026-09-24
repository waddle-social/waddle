import Foundation

/// The XEP-0004 data form of a command response.
public struct ExtensionCommandForm: Sendable, Equatable, Hashable {
    public let title: String?
    public let instructions: String?
    public var fields: [ExtensionCommandField]

    public init(title: String?, instructions: String?, fields: [ExtensionCommandField]) {
        self.title = title
        self.instructions = instructions
        self.fields = fields
    }

    public func field(_ variable: String) -> ExtensionCommandField? {
        fields.first { $0.type != .fixed && $0.variable == variable }
    }
}
