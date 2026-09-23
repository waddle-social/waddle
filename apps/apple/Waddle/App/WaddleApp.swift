import SwiftUI

@main
struct WaddleApp: App {
    @State private var appState = AppState()
    #if os(iOS)
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    #elseif os(macOS)
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    #endif

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(appState)
                .onAppear { appDelegate.attach(appState) }
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
