import Testing

@testable import WaddleKit

struct NotificationPolicyTests {
    @Test(arguments: [true, false])
    func doNotDisturbSuppressesMessageAlertsAndSounds(soundsEnabled: Bool) {
        let policy = NotificationPolicy(kind: .message, isDoNotDisturb: true, messageSoundsEnabled: soundsEnabled)
        #expect(!policy.showAlert)
        #expect(!policy.playSound)
    }

    @Test func messagesCanShowAndSound() {
        let policy = NotificationPolicy(kind: .message, isDoNotDisturb: false)
        #expect(policy.showAlert)
        #expect(policy.playSound)
    }

    @Test func turningOffMessageSoundsKeepsTheAlert() {
        let policy = NotificationPolicy(kind: .message, isDoNotDisturb: false, messageSoundsEnabled: false)
        #expect(policy.showAlert)
        #expect(!policy.playSound)
    }

    @Test func turningOffNotificationsSuppressesMessageAlertsAndSounds() {
        let policy = NotificationPolicy(kind: .message, isDoNotDisturb: false, notificationsEnabled: false)
        #expect(!policy.showAlert)
        #expect(!policy.playSound)
    }

    @Test func doNotDisturbSuppressesIncomingCallBannersAndRingtones() {
        let policy = NotificationPolicy(kind: .incomingCall, isDoNotDisturb: true)
        #expect(!policy.showAlert)
        #expect(!policy.playSound)
    }

    @Test func incomingCallsStillAlertWhenMessageNotificationsAndSoundsAreOff() {
        let policy = NotificationPolicy(
            kind: .incomingCall, isDoNotDisturb: false, notificationsEnabled: false, messageSoundsEnabled: false
        )
        #expect(policy.showAlert)
        #expect(policy.playSound)
    }

    @Test func missedCallPushesKeepTheirAlertUnderDoNotDisturb() {
        let policy = NotificationPolicy(kind: .missedCall, isDoNotDisturb: true, messageSoundsEnabled: false)
        #expect(policy.showAlert)
        #expect(policy.playSound)
    }

    @Test func turningOffNotificationsSuppressesMissedCallPushes() {
        let policy = NotificationPolicy(kind: .missedCall, isDoNotDisturb: false, notificationsEnabled: false)
        #expect(!policy.showAlert)
        #expect(!policy.playSound)
    }
}
