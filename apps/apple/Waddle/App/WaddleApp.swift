import SwiftUI

@main
struct WaddleApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
#if os(macOS)
        .defaultSize(width: AppConfig.desktopWindowWidth, height: AppConfig.desktopWindowHeight)
#endif
    }
}
