import Foundation

extension FileOutboxStore {
    /// The account's outbox at `Application Support/Waddle/Outbox/<key>.json`,
    /// or nil when the platform has no Application Support directory.
    public static func applicationSupport(for account: BareJID) -> FileOutboxStore? {
        guard let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else {
            return nil
        }
        return FileOutboxStore(url: url(for: account, in: base))
    }

    nonisolated static func url(for account: BareJID, in applicationSupport: URL) -> URL {
        applicationSupport
            .appendingPathComponent("Waddle", isDirectory: true)
            .appendingPathComponent("Outbox", isDirectory: true)
            .appendingPathComponent(fileKey(for: account))
            .appendingPathExtension("json")
    }

    /// A reversible, filesystem-safe key: `[a-z0-9.-]` pass through and
    /// every other UTF-8 byte (including `_`) becomes `_` plus two hex
    /// digits, so distinct accounts never share a file.
    nonisolated static func fileKey(for account: BareJID) -> String {
        var key = ""
        for byte in account.description.utf8 {
            if isSafe(byte) {
                key.unicodeScalars.append(Unicode.Scalar(byte))
            } else {
                key += (byte < 0x10 ? "_0" : "_") + String(byte, radix: 16)
            }
        }
        return key
    }

    private nonisolated static func isSafe(_ byte: UInt8) -> Bool {
        switch byte {
        case UInt8(ascii: "a")...UInt8(ascii: "z"), UInt8(ascii: "0")...UInt8(ascii: "9"), UInt8(ascii: "."), UInt8(ascii: "-"):
            return true
        default:
            return false
        }
    }
}
