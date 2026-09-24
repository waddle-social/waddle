import Foundation
import Testing
@testable import WaddleKit

struct SlashActionTests {
    private func run(_ text: String, extensions: [ExtensionCommand] = []) -> SlashAction? {
        guard let trigger = SlashTrigger.parse(text),
              let resolution = SlashCandidates.resolve(prefix: trigger.prefix, extensions: extensions, inRoom: true)
        else { return nil }
        return resolution.action(trailing: trigger.trailing)
    }

    private func command(inlineField: String? = nil, composerExecute: Bool = false) -> ExtensionCommand {
        ExtensionCommand(
            serviceJID: "ext.waddle.test",
            node: "deploy",
            name: "Deploy",
            scope: .global,
            composerPrefix: "deploy",
            inlineField: inlineField,
            composerExecute: composerExecute
        )
    }

    @Test func meSendsCanonicalPrefix() {
        #expect(run("/me waves") == .send("/me waves"))
        #expect(run("/ME   waves") == .send("/me waves"))
        #expect(run("/me waves\nhello") == .send("/me waves\nhello"))
    }

    @Test func meWithoutActionIsIncomplete() {
        #expect(run("/me") == .incomplete)
        #expect(run("/me   ") == .incomplete)
    }

    @Test func shrugAppendsKaomoji() {
        #expect(Shrug.text == "¯\\_(ツ)_/¯")
        #expect(run("/shrug") == .send("¯\\_(ツ)_/¯"))
        #expect(run("/shrug oh well  ") == .send("oh well ¯\\_(ツ)_/¯"))
    }

    @Test func giphyAndAliasSearch() {
        #expect(run("/giphy  happy cat ") == .searchGIFs(query: "happy cat"))
        #expect(run("/gif") == .searchGIFs(query: ""))
    }

    @Test func presenceCommandsIgnoreTrailing() {
        #expect(run("/away") == .setAvailability(.away))
        #expect(run("/active back now") == .setAvailability(.available))
        #expect(run("/DND") == .setAvailability(.doNotDisturb))
    }

    @Test func extensionInlineSubmit() {
        let deploy = command(inlineField: "target")
        #expect(run("/deploy  prod ", extensions: [deploy]) == .runExtension(deploy, .inlineSubmit(field: "target", value: "prod")))
        #expect(run("/deploy", extensions: [deploy]) == .runExtension(deploy, .openForm(prefill: nil)))
    }

    @Test func extensionDirectExecute() {
        let deploy = command(composerExecute: true)
        #expect(run("/deploy", extensions: [deploy]) == .runExtension(deploy, .execute))
        #expect(run("/deploy prod", extensions: [deploy]) == .runExtension(deploy, .openForm(prefill: "prod")))
    }

    @Test func extensionInlineBeatsExecute() {
        let deploy = command(inlineField: "target", composerExecute: true)
        #expect(run("/deploy prod", extensions: [deploy]) == .runExtension(deploy, .inlineSubmit(field: "target", value: "prod")))
        #expect(run("/deploy", extensions: [deploy]) == .runExtension(deploy, .execute))
    }

    @Test func extensionFormKeepsTypedText() {
        let deploy = command()
        #expect(run("/deploy", extensions: [deploy]) == .runExtension(deploy, .openForm(prefill: nil)))
        #expect(run("/deploy prod", extensions: [deploy]) == .runExtension(deploy, .openForm(prefill: "prod")))
    }

    @Test func completionReplacesPrefixAndKeepsTrailing() {
        #expect(SlashCompletion.complete(text: "/sh", with: .builtin(.shrug)) == "/shrug ")
        #expect(SlashCompletion.complete(text: "/", with: .builtin(.me)) == "/me ")
        #expect(SlashCompletion.complete(text: "/ hello", with: .builtin(.shrug)) == "/shrug hello")
        #expect(SlashCompletion.complete(text: "/gi cats", with: .builtin(.giphy)) == "/giphy cats")
        #expect(SlashCompletion.complete(text: "/sh\nline", with: .builtin(.shrug)) == "/shrug \nline")
        #expect(SlashCompletion.complete(text: "not a trigger", with: .builtin(.away)) == "/away ")
    }
}
