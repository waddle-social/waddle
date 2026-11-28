import Testing

@testable import WaddleKit

struct NotificationControlTests {
    @Test(arguments: [true, false])
    func asksForPermissionBeforeTogglingNotifications(notificationsEnabled: Bool) {
        let control = NotificationControl(permission: .undetermined, notificationsEnabled: notificationsEnabled)
        #expect(control == .enable)
        #expect(control.action == .requestPermission)
    }

    @Test(arguments: [true, false])
    func systemDenialDisablesTheBell(notificationsEnabled: Bool) {
        let control = NotificationControl(permission: .denied, notificationsEnabled: notificationsEnabled)
        #expect(control == .blocked)
        #expect(control.action == nil)
    }

    @Test func anAuthorizedBellTogglesThePreferenceUsedByMessageAlerts() {
        let enabled = NotificationControl(permission: .granted, notificationsEnabled: true)
        let disabled = NotificationControl(permission: .granted, notificationsEnabled: false)
        #expect(enabled == .enabled)
        #expect(disabled == .disabled)

        guard case let .setEnabled(afterTurningOff) = enabled.action,
              case let .setEnabled(afterTurningOn) = disabled.action else {
            Issue.record("An authorized bell should toggle notifications")
            return
        }
        let muted = NotificationPolicy(kind: .message, isDoNotDisturb: false, notificationsEnabled: afterTurningOff)
        #expect(!muted.showAlert)
        #expect(!muted.playSound)
        let unmuted = NotificationPolicy(kind: .message, isDoNotDisturb: false, notificationsEnabled: afterTurningOn)
        #expect(unmuted.showAlert)
        #expect(unmuted.playSound)
    }
}
