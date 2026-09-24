import Foundation
import Testing
@testable import WaddleKit

struct SlashCandidatesTests {
    private func command(
        _ prefix: String?,
        node: String = "n",
        scope: ExtensionCommandScope = .global,
        inlineField: String? = nil,
        composerExecute: Bool = false
    ) -> ExtensionCommand {
        ExtensionCommand(
            serviceJID: "ext.waddle.test",
            node: node,
            name: "Command \(prefix ?? "-")",
            scope: scope,
            composerPrefix: prefix,
            inlineField: inlineField,
            composerExecute: composerExecute
        )
    }

    private func names(_ candidates: [SlashCandidate]) -> [String] {
        candidates.map(\.name)
    }

    @Test func builtinsMatchCaseInsensitivelyByNameOrAlias() {
        #expect(BuiltinSlashCommand(keyword: "GIF") == .giphy)
        #expect(BuiltinSlashCommand(keyword: "Me") == .me)
        #expect(BuiltinSlashCommand(keyword: "") == nil)
        #expect(BuiltinSlashCommand(keyword: "gi") == nil)
    }

    @Test func builtinTable() {
        #expect(BuiltinSlashCommand.me.usage == "/me <action>")
        #expect(BuiltinSlashCommand.me.description == "Send an action message, e.g. /me waves")
        #expect(BuiltinSlashCommand.shrug.usage == "/shrug [message]")
        #expect(BuiltinSlashCommand.giphy.usage == "/giphy [search]")
        #expect(BuiltinSlashCommand.giphy.description == "Search for a GIF")
        #expect(BuiltinSlashCommand.giphy.aliases == ["gif"])
        #expect(BuiltinSlashCommand.dnd.description == "Set your status to Do Not Disturb")
    }

    @Test func emptyPrefixListsBuiltinsThenExtensions() {
        let deploy = command("deploy")
        let candidates = SlashCandidates.filter(prefix: "", extensions: [deploy], inRoom: false)
        #expect(names(candidates) == ["me", "shrug", "giphy", "away", "active", "dnd", "deploy"])
        #expect(candidates.last == .extension(deploy))
    }

    @Test func aliasMatchListsBuiltinOnce() {
        let candidates = SlashCandidates.filter(prefix: "G", extensions: [], inRoom: true)
        #expect(candidates == [.builtin(.giphy)])
    }

    @Test func extensionsMatchByPrefixCaseInsensitively() {
        let deploy = command("Deploy")
        let dice = command("dice")
        let other = command("poll")
        let candidates = SlashCandidates.filter(prefix: "d", extensions: [deploy, dice, other], inRoom: true)
        #expect(candidates == [.builtin(.dnd), .extension(deploy), .extension(dice)])
    }

    @Test func extensionsWithoutComposerPrefixAreHidden() {
        let candidates = SlashCandidates.filter(prefix: "", extensions: [command(nil), command("")], inRoom: true)
        #expect(candidates.allSatisfy { if case .builtin = $0 { true } else { false } })
    }

    @Test func channelScopedCommandsOnlyInRooms() {
        let kick = command("kick", scope: .channel)
        #expect(SlashCandidates.filter(prefix: "k", extensions: [kick], inRoom: false).isEmpty)
        #expect(SlashCandidates.filter(prefix: "k", extensions: [kick], inRoom: true) == [.extension(kick)])
        #expect(SlashCandidates.resolve(prefix: "kick", extensions: [kick], inRoom: false) == nil)
        #expect(SlashCandidates.resolve(prefix: "kick", extensions: [kick], inRoom: true) == .extension(kick))
    }

    @Test func builtinsShadowExtensionsWithTheirNamesOrAliases() {
        let gif = command("GIF")
        let shrug = command("shrug")
        let candidates = SlashCandidates.filter(prefix: "", extensions: [gif, shrug], inRoom: true)
        #expect(!candidates.contains(.extension(gif)))
        #expect(!candidates.contains(.extension(shrug)))
        #expect(SlashCandidates.resolve(prefix: "gif", extensions: [gif], inRoom: true) == .builtin(.giphy))
    }

    @Test func resolveIsExactAndCaseInsensitive() {
        let deploy = command("deploy")
        #expect(SlashCandidates.resolve(prefix: "DEPLOY", extensions: [deploy], inRoom: true) == .extension(deploy))
        #expect(SlashCandidates.resolve(prefix: "dep", extensions: [deploy], inRoom: true) == nil)
        #expect(SlashCandidates.resolve(prefix: "", extensions: [deploy], inRoom: true) == nil)
        #expect(SlashCandidates.resolve(prefix: "Shrug", extensions: [], inRoom: false) == .builtin(.shrug))
    }

    @Test func ambiguousExtensionPrefixDoesNotResolve() {
        let first = command("deploy", node: "a")
        let second = command("deploy", node: "b")
        #expect(SlashCandidates.resolve(prefix: "deploy", extensions: [first, second], inRoom: true) == nil)
        #expect(SlashCandidates.filter(prefix: "deploy", extensions: [first, second], inRoom: true).count == 2)
    }

    @Test func candidateDisplay() {
        let deploy = command("deploy")
        #expect(SlashCandidate.builtin(.giphy).usage == "/giphy [search]")
        #expect(SlashCandidate.builtin(.giphy).description == "Search for a GIF")
        #expect(SlashCandidate.extension(deploy).name == "deploy")
        #expect(SlashCandidate.extension(deploy).usage == "/deploy")
        #expect(SlashCandidate.extension(deploy).description == "Command deploy")
    }
}
