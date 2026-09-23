import Foundation

/// RFC 3339 instants as the core renders them (chrono `to_rfc3339`: an
/// offset suffix, and 3, 6 or 9 fractional digits only when non-zero).
enum FFIRFC3339 {
    private static let formatters = Formatters()

    static func date(from raw: String) -> Date? {
        formatters.date(from: millisecondPrecision(raw))
    }

    /// Cuts fractional seconds to milliseconds. `ISO8601DateFormatter` is
    /// not specified to accept more digits, and nothing downstream orders
    /// finer than that.
    static func millisecondPrecision(_ raw: String) -> String {
        guard let dot = raw.firstIndex(of: "."), raw[..<dot].contains("T") else { return raw }
        let digitsStart = raw.index(after: dot)
        let digitsEnd = raw[digitsStart...].firstIndex(where: { !$0.isASCII || !$0.isNumber }) ?? raw.endIndex
        guard raw.distance(from: digitsStart, to: digitsEnd) > 3 else { return raw }
        let kept = raw.index(digitsStart, offsetBy: 3)
        return String(raw[..<kept] + raw[digitsEnd...])
    }

    /// One cached formatter pair. `ISO8601DateFormatter` is not documented
    /// thread-safe on every Foundation the bridge builds against, and FFI
    /// callbacks arrive on arbitrary threads, so parsing is serialized.
    private final class Formatters: @unchecked Sendable {
        private let lock = NSLock()
        private let fractional: ISO8601DateFormatter
        private let whole: ISO8601DateFormatter

        init() {
            fractional = ISO8601DateFormatter()
            fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            whole = ISO8601DateFormatter()
            whole.formatOptions = [.withInternetDateTime]
        }

        func date(from raw: String) -> Date? {
            lock.lock()
            defer { lock.unlock() }
            return fractional.date(from: raw) ?? whole.date(from: raw)
        }
    }
}
