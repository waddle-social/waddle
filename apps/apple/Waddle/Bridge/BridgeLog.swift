import Foundation
#if canImport(os)
import os
#endif

/// Diagnostics for the FFI bridge. Never pass message bodies or tokens.
enum BridgeLog {
    #if canImport(os)
    private static let logger = Logger(subsystem: "social.waddle.ios", category: "FFIBridge")
    #endif

    static func debug(_ message: @autoclosure () -> String) {
        #if canImport(os)
        let text = message()
        logger.debug("\(text, privacy: .private)")
        #endif
    }

    static func error(_ message: @autoclosure () -> String) {
        #if canImport(os)
        let text = message()
        logger.error("\(text, privacy: .private)")
        #endif
    }
}
