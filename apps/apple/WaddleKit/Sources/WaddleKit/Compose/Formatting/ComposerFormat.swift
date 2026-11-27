/// A formatting-bar style. Lists use literal body prefixes. The other
/// styles use markdown that `ComposerMarkdown` turns into XEP-0394 spans.
public enum ComposerFormat: CaseIterable, Hashable, Sendable {
    case bold
    case italic
    case strikethrough
    case orderedList
    case bulletList
    case quote
    case code
    case codeBlock

    public enum PhysicalKey {
        case ansi(UInt16) // AppKit virtual key code
        case usb(Int)     // UIKit keyboard HID usage
    }

    /// The physical 7 and 8 keys used by Cmd-Shift list shortcuts.
    public static func listShortcut(for key: PhysicalKey) -> Self? {
        switch key {
        case .ansi(0x1A), .usb(0x24): return .orderedList
        case .ansi(0x1C), .usb(0x25): return .bulletList
        default: return nil
        }
    }
}
