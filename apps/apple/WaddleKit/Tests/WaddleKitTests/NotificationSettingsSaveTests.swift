import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct NotificationSettingsSaveTests {
    @Test func roomModeFailureKeepsTheOldModeAndExplainsTheAdminFix() async {
        let port = FakePort()
        port.notifySettingsWriteResult = .failure(.notificationSettingsStorageMismatch)
        let session = SessionCoordinator(account: me, port: port)
        let save = NotificationSettingsSave()

        await save.perform(for: roomConversation) {
            try await session.setNotifyMode(.never, for: roomConversation)
        }

        #expect(session.directory.notifyMode(for: roomConversation) == .onMention)
        #expect(save.errorMessage == "This room's settings node was created by an older client. Ask a server admin to delete the node so Waddle can re-create it.")
    }

    @Test func directPreviewFailureUsesPersonalStorageCopyAndCanBeRetried() async {
        let port = FakePort()
        port.notifySettingsWriteResults = [.failure(.notificationSettingsStorageMismatch), .success(())]
        let session = SessionCoordinator(account: me, port: port)
        let conversation = ConversationID.direct(bare("bob@waddle.test"))
        let save = NotificationSettingsSave()

        await save.perform(for: conversation) {
            try await session.setShowsMessagesInPush(true, for: conversation)
        }
        #expect(!session.directory.showsMessagesInPush(for: conversation))
        #expect(save.errorMessage == "Your direct-message notification settings couldn't be saved — their storage was created by an incompatible older client.")

        await save.perform(for: conversation) {
            #expect(save.errorMessage == nil)
            try await session.setShowsMessagesInPush(true, for: conversation)
        }
        #expect(session.directory.showsMessagesInPush(for: conversation))
        #expect(save.errorMessage == nil)
    }

    @Test func otherFailuresUseTheWebRetryMessage() async {
        struct UnexpectedFailure: Error {}
        let save = NotificationSettingsSave()
        let failures: [any Error] = [PortError.rejected, PortError.timeout, UnexpectedFailure()]

        for error in failures {
            await save.perform(for: roomConversation) { throw error }
            #expect(save.errorMessage == "Couldn't save the setting. Try again in a moment.")
        }
    }

    @Test func aSaveBlocksAnotherChangeUntilItSettles() async {
        let save = NotificationSettingsSave()

        await save.perform(for: roomConversation) {
            #expect(save.isSaving)
            await save.perform(for: roomConversation) {
                Issue.record("A second change should wait for the current save to finish")
            }
            #expect(save.isSaving)
        }

        #expect(!save.isSaving)
    }
}
