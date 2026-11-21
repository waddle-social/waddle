import SwiftUI

@main
struct WaddleApp: App {
    @State private var appState = AppState.shared
    #if os(iOS)
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    #elseif os(macOS)
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    #endif

    init() {
        HuddleAppearance.apply()
    }

    var body: some Scene {
        WindowGroup {
            if AppState.isHostingTests {
                // The unit-test bundle runs inside the app: without the root
                // view the host never restores a session or loads providers.
                Color.clear
            } else {
                RootView()
                    .environment(appState)
            }
        }
        #if os(macOS)
        .defaultSize(width: 1_280, height: 840)
        #endif
        .commands { WaddleCommands(appState: appState) }

        #if os(macOS)
        Settings {
            SettingsView()
                .environment(appState)
                .frame(width: 480)
        }
        #endif
    }
}
