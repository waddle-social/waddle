import Testing

@testable import WaddleKit

@Suite("Call screen sources") struct CallScreenSourcesTests {
    private typealias Screen = CallScreenCandidates.Screen
    private typealias Window = CallScreenCandidates.Window

    @Test func oneScreenIsTheEntireScreen() {
        let sources = CallScreenSources(
            CallScreenCandidates(screens: [Screen(number: 7, width: 3024, height: 1964, isMain: true)]), ownBundleID: nil
        )
        #expect(sources.screens == [
            CallScreenSource(id: "screen:7", kind: .screen, title: "Entire screen", detail: "3024 × 1964"),
        ])
    }

    @Test func severalScreensAreNumberedFromTheMainOne() {
        let sources = CallScreenSources(
            CallScreenCandidates(screens: [
                Screen(number: 3, width: 1920, height: 1080, isMain: false),
                Screen(number: 1, width: 3024, height: 1964, isMain: true),
            ]),
            ownBundleID: nil
        )
        #expect(sources.screens.map(\.id) == ["screen:1", "screen:3"])
        #expect(sources.screens.map(\.title) == ["Screen 1", "Screen 2"])
    }

    @Test func onlyOrdinaryTitledWindowsOnScreenAreOffered() {
        let sources = CallScreenSources(
            CallScreenCandidates(windows: [
                Window(number: 1, title: "Budget.numbers", application: "Numbers", bundleID: "com.apple.Numbers"),
                Window(number: 2, title: "", application: "Finder", bundleID: "com.apple.finder"),
                Window(number: 3, title: nil, application: "Dock", bundleID: "com.apple.dock"),
                Window(number: 4, title: "Menu", application: "Safari", bundleID: "com.apple.Safari", layer: 25),
                Window(number: 5, title: "Hidden", application: "Notes", bundleID: "com.apple.Notes", isOnScreen: false),
                Window(number: 6, title: "Palette", application: "Sketch", bundleID: "com.sketch", width: 80, height: 300),
            ]),
            ownBundleID: nil
        )
        #expect(sources.windows == [
            CallScreenSource(id: "window:1", kind: .window, title: "Budget.numbers", detail: "Numbers"),
        ])
    }

    @Test func ourOwnWindowsAreNotOffered() {
        let sources = CallScreenSources(
            CallScreenCandidates(windows: [
                Window(number: 1, title: "Waddle", application: "Waddle", bundleID: "social.waddle.app"),
                Window(number: 2, title: "Inbox", application: "Mail", bundleID: "com.apple.mail"),
            ]),
            ownBundleID: "social.waddle.app"
        )
        #expect(sources.windows.map(\.title) == ["Inbox"])
    }

    @Test func windowsAreListedByApplicationThenTitleAndKeepTheirPlaces() {
        let sources = CallScreenSources(
            CallScreenCandidates(windows: [
                Window(number: 9, title: "notes.md", application: "Zed", bundleID: "dev.zed"),
                Window(number: 4, title: "Untitled", application: "safari", bundleID: "com.apple.Safari"),
                Window(number: 2, title: "Untitled", application: "Safari", bundleID: "com.apple.Safari"),
                Window(number: 5, title: "apple.com", application: "Safari", bundleID: "com.apple.Safari"),
            ]),
            ownBundleID: nil
        )
        #expect(sources.windows.map(\.id) == ["window:5", "window:2", "window:4", "window:9"])
    }

    @Test func nothingToShareIsEmpty() {
        #expect(CallScreenSources(CallScreenCandidates(), ownBundleID: nil).isEmpty)
    }
}
