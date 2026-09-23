import Foundation
#if canImport(os)
import os
#endif

/// Outbox diagnostics. Never pass message bodies.
enum OutboxLog {
    #if canImport(os)
    private static let logger = Logger(subsystem: "social.waddle.ios", category: "Outbox")
    #endif

    static func error(_ message: @autoclosure () -> String) {
        #if canImport(os)
        let text = message()
        logger.error("\(text, privacy: .private)")
        #endif
    }
}
