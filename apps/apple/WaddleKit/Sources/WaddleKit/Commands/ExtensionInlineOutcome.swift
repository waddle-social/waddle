import Foundation

/// What running a command inline from the composer led to.
public enum ExtensionInlineOutcome: Sendable, Equatable {
    /// The command completed or was canceled, on invoke or after the
    /// inline value completed its single-stage form.
    case finished(ExtensionCommandResult)
    /// The command is still executing and needs the palette: a
    /// multi-stage flow, a blocked field, or other required fields. A
    /// form containing the inline field is prefilled with its value.
    case needsInput(ExtensionCommandResult)
}
