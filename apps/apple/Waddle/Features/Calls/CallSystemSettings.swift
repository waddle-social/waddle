import SwiftUI
import WaddleKit

/// Opens the system's settings at the place where a call's device is allowed.
@MainActor
enum CallSystemSettings {
    static func open(for kind: CallMediaIssue.Kind) {
        #if os(macOS)
        let pane = switch kind {
        case .microphone: "Privacy_Microphone"
        case .camera: "Privacy_Camera"
        case .screen: "Privacy_ScreenCapture"
        }
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") {
            NSWorkspace.shared.open(url)
        }
        #else
        // iOS has one page for the app, with every switch on it.
        if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
        #endif
    }
}
