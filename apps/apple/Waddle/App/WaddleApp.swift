import SwiftUI

@main
struct WaddleApp: App {
    #if os(iOS)
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    #elseif os(macOS)
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    #endif

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
#if os(macOS)
        .defaultSize(width: AppConfig.desktopWindowWidth, height: AppConfig.desktopWindowHeight)
#endif
    }
}
