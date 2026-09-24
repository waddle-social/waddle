import Foundation
import Testing
@testable import WaddleKit

private let poll = ExtensionCommand(
    serviceJID: "extensions.waddle.test",
    node: "urn:waddle:extension:1:poll",
    name: "Poll",
    scope: .channel,
    composerPrefix: "poll",
    inlineField: "question",
    composerExecute: false
)

private let ping = ExtensionCommand(
    serviceJID: "extensions.waddle.test",
    node: "urn:waddle:extension:1:ping",
    name: "Ping",
    scope: .global,
    composerPrefix: "ping",
    inlineField: nil,
    composerExecute: true
)

private func field(
    _ variable: String,
    _ type: ExtensionFieldType = .textSingle,
    required: Bool = false,
    blocked: Bool = false,
    values: [String] = []
) -> ExtensionCommandField {
    ExtensionCommandField(
        variable: variable,
        label: nil,
        type: type,
        required: required,
        blocked: blocked,
        options: [],
        values: values
    )
}

private func stage(
    _ fields: [ExtensionCommandField],
    actions: [ExtensionCommandAction] = [.complete, .cancel],
    sessionID: String? = "s1"
) -> ExtensionCommandResult {
    ExtensionCommandResult(
        status: .executing,
        sessionID: sessionID,
        actions: actions,
        form: ExtensionCommandForm(title: "Poll", instructions: nil, fields: fields),
        notes: []
    )
}

private let done = ExtensionCommandResult(
    status: .completed,
    sessionID: "s1",
    actions: [],
    form: nil,
    notes: [ExtensionCommandNote(type: .info, text: "Posted")]
)

@MainActor
@Suite("Extension commands (XEP-0050)")
struct ExtensionCommandTests {
    private func online(_ port: FakePort = FakePort()) -> (SessionCoordinator, FakePort) {
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        return (coordinator, port)
    }

    // MARK: - Discovery

    @Test func readySessionDiscoversCommands() async {
        let port = FakePort()
        port.extensionCommandDiscovery = .success([poll, ping])
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        port.emit(.connected)
        await eventually { !coordinator.extensionCommands.isEmpty }
        await coordinator.readyTask?.value
        #expect(coordinator.extensionCommands == [poll, ping])
        #expect(port.extensionDiscoveryCount == 1)
        await coordinator.stop()
        #expect(coordinator.extensionCommands.isEmpty)
    }

    @Test func refreshReplacesTheCachedList() async {
        let (coordinator, port) = online()
        port.extensionCommandDiscovery = .success([poll])
        await coordinator.refreshExtensionCommands()
        #expect(coordinator.extensionCommands == [poll])
        port.extensionCommandDiscovery = .success([ping])
        await coordinator.refreshExtensionCommands()
        #expect(coordinator.extensionCommands == [ping])
        #expect(port.extensionDiscoveryCount == 2)
    }

    @Test func failedRefreshKeepsThePreviousList() async {
        let (coordinator, port) = online()
        port.extensionCommandDiscovery = .success([poll])
        await coordinator.refreshExtensionCommands()
        port.extensionCommandDiscovery = .failure(.timeout)
        await coordinator.refreshExtensionCommands()
        #expect(coordinator.extensionCommands == [poll])
    }

    @Test func offlineRefreshDoesNotQuery() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)
        await coordinator.refreshExtensionCommands()
        #expect(port.extensionDiscoveryCount == 0)
    }

    @Test func discoveryAnsweredAfterADropIsDiscarded() async {
        let (coordinator, port) = online()
        port.extensionCommandDiscovery = .success([poll])
        port.holdsExtensionDiscovery = true
        let refresh = Task { await coordinator.refreshExtensionCommands() }
        await eventually { port.extensionDiscoveryCount == 1 }
        coordinator.handle(.disconnected)
        port.releaseExtensionDiscovery()
        await refresh.value
        #expect(coordinator.extensionCommands.isEmpty)
        await coordinator.stop()
    }

    // MARK: - Invoke and submit

    @Test func runInvokesInTheRoom() async throws {
        let (coordinator, port) = online()
        port.invokeResult = .success(done)
        let result = try await coordinator.runExtensionCommand(ping, room: room)
        #expect(result == done)
        #expect(port.invocations.count == 1)
        #expect(port.invocations[0].command == ping)
        #expect(port.invocations[0].room == room)
    }

    @Test func runPropagatesPortErrors() async {
        let (coordinator, port) = online()
        port.invokeResult = .failure(.rejected)
        await #expect(throws: PortError.rejected) {
            try await coordinator.runExtensionCommand(ping, room: nil)
        }
    }

    @Test func runOfflineThrowsWithoutInvoking() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)
        await #expect(throws: PortError.notConnected) {
            try await coordinator.runExtensionCommand(ping, room: nil)
        }
        #expect(port.invocations.isEmpty)
    }

    @Test func submitSendsEditedValuesWithoutFixedFields() async throws {
        let (coordinator, port) = online()
        port.submitResult = .success(done)
        var current = stage([
            field("", .fixed, values: ["Ask the room"]),
            field("question", required: true),
            field("choices", .textMulti, values: ["a", "b"]),
            field("anonymous", .boolean, values: ["0"]),
            field("token", .hidden, values: ["t1", "t2"]),
            field("colour", .listSingle, values: ["red", "blue"]),
        ])
        current.form = current.form?.setting(["Lunch?"], for: "question")
        let result = try await coordinator.submitExtensionCommand(poll, continuing: current, action: .complete, room: room)
        #expect(result == done)
        #expect(port.submissions == [FakePort.ExtensionSubmission(
            command: poll,
            sessionID: "s1",
            values: [
                ExtensionFormValue(variable: "question", values: ["Lunch?"]),
                ExtensionFormValue(variable: "choices", values: ["a", "b"]),
                ExtensionFormValue(variable: "anonymous", values: ["0"]),
                ExtensionFormValue(variable: "token", values: ["t1", "t2"]),
                ExtensionFormValue(variable: "colour", values: ["red"]),
            ],
            action: .complete,
            room: room
        )])
    }

    @Test func submitRefusesAMissingRequiredField() async {
        let (coordinator, port) = online()
        let current = stage([field("question", required: true, values: ["  "]), field("secret", .hidden, required: true)])
        await #expect(throws: ExtensionCommandSubmitError.missingRequiredFields(variables: ["question"])) {
            try await coordinator.submitExtensionCommand(poll, continuing: current, action: .next, room: nil)
        }
        #expect(port.submissions.isEmpty)
    }

    @Test func submitRefusesABlockedField() async {
        let (coordinator, port) = online()
        let current = stage([field("question", values: ["q"]), field("api_key", .textPrivate, blocked: true)])
        await #expect(throws: ExtensionCommandSubmitError.forbiddenField(variable: "api_key")) {
            try await coordinator.submitExtensionCommand(poll, continuing: current, action: .complete, room: nil)
        }
        #expect(port.submissions.isEmpty)
    }

    @Test func cancelCarriesNoFormEvenWhenBlocked() async throws {
        let (coordinator, port) = online()
        port.submitResult = .success(ExtensionCommandResult(status: .canceled, sessionID: "s1", actions: [], form: nil, notes: []))
        let current = stage([field("question", required: true), field("password", .textPrivate, blocked: true)])
        let result = try await coordinator.submitExtensionCommand(poll, continuing: current, action: .cancel, room: nil)
        #expect(result.status == .canceled)
        #expect(port.submissions.map(\.values) == [[]])
        #expect(port.submissions.map(\.action) == [.cancel])
    }

    @Test func submitPropagatesPortErrors() async {
        let (coordinator, port) = online()
        port.submitResult = .failure(.timeout)
        await #expect(throws: PortError.timeout) {
            try await coordinator.submitExtensionCommand(poll, continuing: stage([]), action: .complete, room: nil)
        }
    }

    // MARK: - Inline

    @Test func inlineValueCompletesASingleStageForm() async throws {
        let (coordinator, port) = online()
        port.invokeResult = .success(stage([field("question", required: true), field("anonymous", .boolean, values: ["0"])]))
        port.submitResult = .success(done)
        let outcome = try await coordinator.runInline(poll, field: "question", value: "Lunch?", room: room)
        #expect(outcome == .finished(done))
        #expect(port.invocations.map(\.room) == [room])
        #expect(port.submissions == [FakePort.ExtensionSubmission(
            command: poll,
            sessionID: "s1",
            values: [
                ExtensionFormValue(variable: "question", values: ["Lunch?"]),
                ExtensionFormValue(variable: "anonymous", values: ["0"]),
            ],
            action: .complete,
            room: room
        )])
    }

    @Test func inlineWithoutCompleteReturnsThePrefilledStage() async throws {
        let (coordinator, port) = online()
        port.invokeResult = .success(stage([field("question")], actions: [.next, .cancel]))
        let outcome = try await coordinator.runInline(poll, field: "question", value: "Lunch?", room: nil)
        guard case let .needsInput(result) = outcome else {
            Issue.record("expected the palette, got \(outcome)")
            return
        }
        #expect(result.form?.field("question")?.values == ["Lunch?"])
        #expect(port.submissions.isEmpty)
    }

    @Test func inlineWithAnotherEmptyRequiredFieldNeedsInput() async throws {
        let (coordinator, port) = online()
        port.invokeResult = .success(stage([field("question"), field("choices", .textMulti, required: true)]))
        let outcome = try await coordinator.runInline(poll, field: "question", value: "Lunch?", room: nil)
        guard case let .needsInput(result) = outcome else {
            Issue.record("expected the palette, got \(outcome)")
            return
        }
        #expect(result.form?.field("question")?.values == ["Lunch?"])
        #expect(port.submissions.isEmpty)
    }

    @Test func inlineNeverFillsOrSubmitsABlockedField() async throws {
        let (coordinator, port) = online()
        port.invokeResult = .success(stage([field("token", .textPrivate, blocked: true)]))
        let outcome = try await coordinator.runInline(poll, field: "token", value: "hunter2", room: nil)
        guard case let .needsInput(result) = outcome else {
            Issue.record("expected the palette, got \(outcome)")
            return
        }
        #expect(result.form?.field("token")?.values == [])
        #expect(port.submissions.isEmpty)
    }

    @Test func inlineFieldMissingFromTheFormNeedsInput() async throws {
        let (coordinator, port) = online()
        let invoked = stage([field("topic")])
        port.invokeResult = .success(invoked)
        let outcome = try await coordinator.runInline(poll, field: "question", value: "Lunch?", room: nil)
        #expect(outcome == .needsInput(invoked))
        #expect(port.submissions.isEmpty)
    }

    @Test func inlineCommandThatCompletesOnInvokeIsFinished() async throws {
        let (coordinator, port) = online()
        port.invokeResult = .success(done)
        let outcome = try await coordinator.runInline(ping, field: "question", value: "x", room: nil)
        #expect(outcome == .finished(done))
        #expect(port.submissions.isEmpty)
    }

    @Test func inlineSubmitThatOpensANextStageNeedsInput() async throws {
        let (coordinator, port) = online()
        port.invokeResult = .success(stage([field("question")]))
        let next = stage([field("choices", .textMulti, required: true)], sessionID: "s1")
        port.submitResult = .success(next)
        let outcome = try await coordinator.runInline(poll, field: "question", value: "Lunch?", room: nil)
        #expect(outcome == .needsInput(next))
    }

    @Test func inlinePropagatesSubmitErrors() async {
        let (coordinator, port) = online()
        port.invokeResult = .success(stage([field("question")]))
        port.submitResult = .failure(.rejected)
        await #expect(throws: PortError.rejected) {
            try await coordinator.runInline(poll, field: "question", value: "Lunch?", room: nil)
        }
    }

    // MARK: - Form rules

    @Test func requiredBooleanAndMultiValueRules() {
        let form = ExtensionCommandForm(title: nil, instructions: nil, fields: [
            field("consent", .boolean, required: true, values: ["0"]),
            field("tags", .listMulti, required: true, values: ["", " x "]),
            field("note", .fixed, required: true),
            field("jid", .jidSingle, required: true, values: ["", "a@b"]),
        ])
        #expect(form.missingRequiredFields.map(\.variable) == ["jid"])
    }

    @Test func settingSkipsFixedAndBlockedFields() {
        let form = ExtensionCommandForm(title: nil, instructions: nil, fields: [
            field("label", .fixed, values: ["Heading"]),
            field("password", .textPrivate, blocked: true),
        ])
        #expect(form.setting(["x"], for: "label") == form)
        #expect(form.setting(["x"], for: "password") == form)
    }

    @Test func pendingFormRequiresAnExecutingSession() {
        let form = stage([field("question")])
        #expect(form.pendingForm != nil)
        #expect(stage([field("question")], sessionID: nil).pendingForm == nil)
        #expect(stage([]).pendingForm == nil)
        #expect(done.pendingForm == nil)
    }
}
