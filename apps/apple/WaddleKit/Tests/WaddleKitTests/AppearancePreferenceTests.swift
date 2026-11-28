import Testing
import WaddleKit

struct AppearancePreferenceTests {
    @Test(arguments: [
        (AppearancePreference.system, AppearancePreference.dark),
        (.dark, .light),
        (.light, .system),
    ])
    func advancesToTheNextThemeInWebMenuOrder(current: AppearancePreference, next: AppearancePreference) {
        #expect(current.next == next)
    }
}
