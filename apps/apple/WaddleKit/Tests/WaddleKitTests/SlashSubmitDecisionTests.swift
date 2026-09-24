import Foundation
import Testing
@testable import WaddleKit

struct SlashSubmitDecisionTests {
    private let poll = ExtensionCommand(
        serviceJID: "ext.waddle.test",
        node: "poll",
        name: "Poll",
        scope: .channel,
        composerPrefix: "poll",
        inlineField: "question",
        composerExecute: false
    )

    private func decide(_ text: String, inRoom: Bool = true) -> SlashSubmitDecision {
        SlashSubmitDecision.decide(text: text, extensions: [poll], inRoom: inRoom)
    }

    @Test func plainTextIsSentAsTyped() {
        #expect(decide("hello") == .sendAsTyped)
        #expect(decide(" /me waves") == .sendAsTyped)
        #expect(decide("/usr/bin") == .sendAsTyped)
    }

    @Test func resolvedCommandsRun() {
        #expect(decide("/shrug ok") == .run(.send("ok ¯\\_(ツ)_/¯")))
        #expect(decide("/away") == .run(.setAvailability(.away)))
        #expect(decide("/gif cats") == .run(.searchGIFs(query: "cats")))
        #expect(decide("/poll Lunch?") == .run(.runExtension(poll, .inlineSubmit(field: "question", value: "Lunch?"))))
    }

    @Test func incompleteCommandCompletes() {
        #expect(decide("/me") == .complete(.builtin(.me)))
        #expect(decide("/ME  ") == .complete(.builtin(.me)))
    }

    @Test func singlePartialMatchCompletes() {
        #expect(decide("/shr") == .complete(.builtin(.shrug)))
        #expect(decide("/po") == .complete(.extension(poll)))
    }

    @Test func ambiguousOrEmptyPrefixAsksToChoose() {
        #expect(decide("/") == .choose)
        #expect(decide("/ hello") == .choose)
        // `a` matches both /away and /active.
        #expect(decide("/a") == .choose)
    }

    @Test func unknownCommandIsReported() {
        #expect(decide("/nope") == .unknown(command: "nope"))
        #expect(decide("/etc is a folder") == .unknown(command: "etc"))
        // Channel-scoped commands do not exist in a 1:1 conversation.
        #expect(decide("/poll Lunch?", inRoom: false) == .unknown(command: "poll"))
    }
}

struct SlashPopoverTests {
    @Test func showsWhileTheCommandWordIsTyped() {
        #expect(SlashPopover.prefix(in: "/") == "")
        #expect(SlashPopover.prefix(in: "/sh") == "sh")
        #expect(SlashPopover.prefix(in: "/ hello") == "")
    }

    @Test func hidesOnceTheCommandIsChosenOrForPlainText() {
        #expect(SlashPopover.prefix(in: "/shrug ") == nil)
        #expect(SlashPopover.prefix(in: "/shrug hi") == nil)
        #expect(SlashPopover.prefix(in: "hello") == nil)
        #expect(SlashPopover.prefix(in: "/1") == nil)
    }
}
